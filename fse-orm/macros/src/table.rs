//! `#[derive(Table)]` expansion: compile-time-checked CRUD.
//!
//! Every generated method body is a `sqlx::query!`/`query_scalar!` call with
//! a *literal* SQL string built at expansion time, so sqlx itself verifies
//! each query against the real database schema — the ORM never becomes the
//! checker. Column reads use typed overrides (`col as "col!: Type"`); json
//! columns come back as TEXT and go through the serde helpers in `fse_orm`.

use fse_schema::{ColumnDef, DefaultValue};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

struct Col {
    def: ColumnDef,
    ident: syn::Ident,
    /// Field type with `Option` stripped.
    inner_ty: syn::Type,
    /// Field type as written.
    field_ty: syn::Type,
}

/// Hoist bind expressions into `let` locals. `sqlx::query!` internally takes
/// `&(expr)`, so a method-call argument (`self.name.as_str()`) would be a
/// temporary that dies before the query runs; a named local outlives it.
fn hoist_binds(exprs: Vec<TokenStream>) -> (Vec<TokenStream>, Vec<syn::Ident>) {
    let mut locals = Vec::new();
    let mut names = Vec::new();
    for (i, expr) in exprs.into_iter().enumerate() {
        let name = format_ident!("bind_{i}");
        locals.push(quote! { let #name = #expr; });
        names.push(name);
    }
    (locals, names)
}

pub fn expand(item: &syn::ItemStruct) -> syn::Result<TokenStream> {
    let span = item.ident.span();
    let table = fse_schema::parse::table_from_struct(item, None)
        .map_err(|e| syn::Error::new(span, e.message))?;

    let syn::Fields::Named(fields) = &item.fields else {
        return Err(syn::Error::new(span, "Table needs named fields"));
    };
    let mut cols: Vec<Col> = Vec::new();
    for (def, field) in table.columns.iter().zip(&fields.named) {
        cols.push(Col {
            def: def.clone(),
            ident: field.ident.clone().expect("named field"),
            inner_ty: syn::parse_str(&def.rust_type)
                .map_err(|e| syn::Error::new(span, format!("{}: {e}", def.rust_type)))?,
            field_ty: field.ty.clone(),
        });
    }

    let ident = &item.ident;
    let vis = &item.vis;
    let table_name = &table.name;
    let auto = table.auto_id();
    let pk_cols: Vec<&Col> = cols.iter().filter(|c| c.def.primary_key).collect();
    let non_pk_cols: Vec<&Col> = cols.iter().filter(|c| !c.def.primary_key).collect();
    // The insert companion carries every column except an autoincrement id.
    let insert_cols: Vec<&Col> = if auto {
        non_pk_cols.clone()
    } else {
        cols.iter().collect()
    };

    let select_list = crate::codegen::select_list(&table.columns);
    let build_fields = crate::codegen::build_fields(&table.columns);

    let pk_params: Vec<TokenStream> = pk_cols.iter().map(|c| fn_param(c)).collect();
    let (pk_bind_locals, pk_bind_names) =
        hoist_binds(pk_cols.iter().map(|c| param_bind(c)).collect());
    let pk_where = pk_cols
        .iter()
        .map(|c| format!("{} = ?", c.def.name))
        .collect::<Vec<_>>()
        .join(" AND ");

    let fetch_sql = format!("SELECT {select_list} FROM {table_name} WHERE {pk_where}");
    let fetch_all_sql = format!("SELECT {select_list} FROM {table_name}");
    let count_sql = format!("SELECT COUNT(*) as \"count!: i64\" FROM {table_name}");
    let delete_sql = format!("DELETE FROM {table_name} WHERE {pk_where}");

    let mut methods = vec![quote! {
        pub async fn fetch(
            db: impl ::sqlx::SqliteExecutor<'_>,
            #(#pk_params),*
        ) -> ::sqlx::Result<Option<Self>> {
            #(#pk_bind_locals)*
            let row = ::sqlx::query!(#fetch_sql, #(#pk_bind_names),*).fetch_optional(db).await?;
            match row {
                Some(r) => Ok(Some(Self { #(#build_fields),* })),
                None => Ok(None),
            }
        }

        pub async fn fetch_all(db: impl ::sqlx::SqliteExecutor<'_>) -> ::sqlx::Result<Vec<Self>> {
            let rows = ::sqlx::query!(#fetch_all_sql).fetch_all(db).await?;
            rows.into_iter()
                .map(|r| -> ::sqlx::Result<Self> { Ok(Self { #(#build_fields),* }) })
                .collect()
        }

        pub async fn count(db: impl ::sqlx::SqliteExecutor<'_>) -> ::sqlx::Result<i64> {
            ::sqlx::query_scalar!(#count_sql).fetch_one(db).await
        }

        pub async fn delete(
            db: impl ::sqlx::SqliteExecutor<'_>,
            #(#pk_params),*
        ) -> ::sqlx::Result<u64> {
            #(#pk_bind_locals)*
            let result = ::sqlx::query!(#delete_sql, #(#pk_bind_names),*).execute(db).await?;
            Ok(result.rows_affected())
        }
    }];

    // One finder per unique column.
    for c in cols.iter().filter(|c| c.def.unique && !c.def.primary_key) {
        let method = format_ident!("fetch_by_{}", c.ident);
        let param = fn_param(c);
        let (bind_local, bind_name) = hoist_binds(vec![param_bind(c)]);
        let sql = format!("SELECT {select_list} FROM {table_name} WHERE {} = ?", c.def.name);
        methods.push(quote! {
            pub async fn #method(
                db: impl ::sqlx::SqliteExecutor<'_>,
                #param
            ) -> ::sqlx::Result<Option<Self>> {
                #(#bind_local)*
                let row = ::sqlx::query!(#sql, #(#bind_name),*).fetch_optional(db).await?;
                match row {
                    Some(r) => Ok(Some(Self { #(#build_fields),* })),
                    None => Ok(None),
                }
            }
        });
    }

    // Full-row UPDATE by pk (skipped for pk-only tables, e.g. join tables).
    if !non_pk_cols.is_empty() {
        let set_list = non_pk_cols
            .iter()
            .map(|c| format!("{} = ?", c.def.name))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("UPDATE {table_name} SET {set_list} WHERE {pk_where}");
        let locals: Vec<TokenStream> = non_pk_cols.iter().filter_map(|c| json_local(c)).collect();
        let (bind_locals, bind_names) = hoist_binds(
            non_pk_cols
                .iter()
                .map(|c| bind_expr(c))
                .chain(pk_cols.iter().map(|c| bind_expr(c)))
                .collect(),
        );
        methods.push(quote! {
            pub async fn update(&self, db: impl ::sqlx::SqliteExecutor<'_>) -> ::sqlx::Result<()> {
                #(#locals)*
                #(#bind_locals)*
                ::sqlx::query!(#sql, #(#bind_names),*).execute(db).await?;
                Ok(())
            }
        });
    }

    let insert_companion = insert_companion(ident, vis, &table.name, &select_list, &build_fields, &insert_cols)?;

    Ok(quote! {
        impl #ident {
            pub const TABLE: &'static str = #table_name;

            #(#methods)*
        }

        #insert_companion
    })
}

/// The `InsertX` companion: every insertable column as a public field, a
/// `new(...)` constructor taking the fields without a `#[orm(default)]`, and
/// an `insert` that returns the full row via `RETURNING`.
fn insert_companion(
    ident: &syn::Ident,
    vis: &syn::Visibility,
    table_name: &str,
    select_list: &str,
    build_fields: &[TokenStream],
    insert_cols: &[&Col],
) -> syn::Result<TokenStream> {
    let insert_ident = format_ident!("Insert{}", ident);

    let struct_fields: Vec<TokenStream> = insert_cols
        .iter()
        .map(|c| {
            let id = &c.ident;
            let ty = &c.field_ty;
            quote! { pub #id: #ty }
        })
        .collect();

    let required: Vec<&&Col> = insert_cols.iter().filter(|c| c.def.default.is_none()).collect();
    let defaulted: Vec<&&Col> = insert_cols.iter().filter(|c| c.def.default.is_some()).collect();
    let new_params: Vec<TokenStream> = required
        .iter()
        .map(|c| {
            let id = &c.ident;
            let ty = &c.field_ty;
            quote! { #id: #ty }
        })
        .collect();
    let required_names: Vec<&syn::Ident> = required.iter().map(|c| &c.ident).collect();
    let default_inits: Vec<TokenStream> = defaulted
        .iter()
        .map(|c| {
            let id = &c.ident;
            let value = default_tokens(c)?;
            Ok(quote! { #id: #value })
        })
        .collect::<syn::Result<_>>()?;

    let sql = if insert_cols.is_empty() {
        format!("INSERT INTO {table_name} DEFAULT VALUES RETURNING {select_list}")
    } else {
        let names = insert_cols.iter().map(|c| c.def.name.as_str()).collect::<Vec<_>>().join(", ");
        let marks = vec!["?"; insert_cols.len()].join(", ");
        format!("INSERT INTO {table_name} ({names}) VALUES ({marks}) RETURNING {select_list}")
    };
    let locals: Vec<TokenStream> = insert_cols.iter().filter_map(|c| json_local(c)).collect();
    let (bind_locals, bind_names) =
        hoist_binds(insert_cols.iter().map(|c| bind_expr(c)).collect());

    Ok(quote! {
        #vis struct #insert_ident {
            #(#struct_fields),*
        }

        impl #insert_ident {
            pub fn new(#(#new_params),*) -> Self {
                Self {
                    #(#required_names,)*
                    #(#default_inits,)*
                }
            }

            pub async fn insert(self, db: impl ::sqlx::SqliteExecutor<'_>) -> ::sqlx::Result<#ident> {
                #(#locals)*
                #(#bind_locals)*
                let r = ::sqlx::query!(#sql #(, #bind_names)*).fetch_one(db).await?;
                Ok(#ident { #(#build_fields),* })
            }
        }
    })
}

/// json columns are serialized into a local before the query so the bind is
/// a plain string.
fn json_local(c: &Col) -> Option<TokenStream> {
    if !c.def.json {
        return None;
    }
    let var = format_ident!("json_{}", c.ident);
    let id = &c.ident;
    Some(if c.def.nullable {
        quote! { let #var = ::fse_orm::opt_to_json_string(self.#id.as_ref())?; }
    } else {
        quote! { let #var = ::fse_orm::to_json_string(&self.#id)?; }
    })
}

/// The expression bound for a column when writing `self` (update/insert).
/// Everything is bound by reference or by `Copy`, so one generator serves
/// both `&self` and owned-`self` methods.
fn bind_expr(c: &Col) -> TokenStream {
    let id = &c.ident;
    if c.def.json {
        let var = format_ident!("json_{}", c.ident);
        return if c.def.nullable {
            quote! { #var.as_deref() }
        } else {
            quote! { #var.as_str() }
        };
    }
    if c.def.is_enum {
        return if c.def.nullable {
            quote! { self.#id.as_ref().map(|v| v.as_str()) }
        } else {
            quote! { self.#id.as_str() }
        };
    }
    match c.def.rust_type.as_str() {
        "String" => {
            if c.def.nullable {
                quote! { self.#id.as_deref() }
            } else {
                quote! { self.#id.as_str() }
            }
        }
        "Vec<u8>" => {
            if c.def.nullable {
                quote! { self.#id.as_deref() }
            } else {
                quote! { self.#id.as_slice() }
            }
        }
        _ => quote! { self.#id },
    }
}

/// Function parameter for a pk/unique lookup: borrow what has a borrowed
/// form, take the rest by value.
fn fn_param(c: &Col) -> TokenStream {
    let id = &c.ident;
    match c.def.rust_type.as_str() {
        "String" => quote! { #id: &str },
        "Vec<u8>" => quote! { #id: &[u8] },
        _ => {
            let ty = &c.inner_ty;
            quote! { #id: #ty }
        }
    }
}

fn param_bind(c: &Col) -> TokenStream {
    let id = &c.ident;
    if c.def.is_enum {
        quote! { #id.as_str() }
    } else {
        quote! { #id }
    }
}

/// The Rust expression for a `#[orm(default = ...)]` value, used by
/// `InsertX::new`. Numeric literals get an `as _` so they adapt to the field
/// width; text defaults on enum columns become the variant itself.
fn default_tokens(c: &Col) -> syn::Result<TokenStream> {
    let span = c.ident.span();
    let value = match c.def.default.as_ref().expect("defaulted column") {
        DefaultValue::Now => match c.def.rust_type.as_str() {
            "NaiveDateTime" => quote! { ::chrono::Utc::now().naive_utc() },
            t if t.starts_with("DateTime") => quote! { ::chrono::Utc::now() },
            other => {
                return Err(syn::Error::new(
                    span,
                    format!("default = now needs NaiveDateTime or DateTime<Utc>, not {other}"),
                ));
            }
        },
        DefaultValue::Int(i) => quote! { (#i as _) },
        DefaultValue::Float(f) => quote! { (#f as _) },
        DefaultValue::Bool(b) => quote! { #b },
        DefaultValue::Text(s) => {
            if c.def.is_enum {
                let ty = &c.inner_ty;
                let variant = format_ident!("{}", upper_camel(s));
                quote! { #ty::#variant }
            } else {
                quote! { #s.to_string() }
            }
        }
    };
    Ok(if c.def.nullable {
        quote! { Some(#value) }
    } else {
        value
    })
}

fn upper_camel(snake: &str) -> String {
    snake
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}
