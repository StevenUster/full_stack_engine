//! `#[derive(Table)]` expansion: compile-time-checked CRUD.
//!
//! Every generated method body is a `sqlx::query!`/`query_scalar!` call with
//! a *literal* SQL string built at expansion time, so sqlx itself verifies
//! each query against the real database schema — the ORM never becomes the
//! checker. Column reads use typed overrides (`col as "col!: Type"`); json
//! columns come back as TEXT and go through the serde helpers in `fse_orm`.

use fse_schema::ColumnDef;
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
    // Relation fields (`#[orm(relation = fk)]`) are not columns: they are
    // skipped in the column zip below and default to `None` in every
    // constructor, populated only by an `include:` join.
    let relation_names: std::collections::HashSet<&str> =
        table.relations.iter().map(|r| r.field.as_str()).collect();
    let column_fields: Vec<&syn::Field> = fields
        .named
        .iter()
        .filter(|f| {
            f.ident
                .as_ref()
                .is_none_or(|id| !relation_names.contains(id.to_string().as_str()))
        })
        .collect();
    let relation_inits: Vec<TokenStream> = table
        .relations
        .iter()
        .map(|r| {
            let id = format_ident!("{}", r.field);
            quote! { #id: None }
        })
        .collect();
    // One hidden `__fse_relation_<field>` associated function per relation,
    // used by `find!`/`find_one!`'s `include:` — see relation.rs for why this
    // has to be generated here (in the relation's own owning struct) rather
    // than at the `find!` call site.
    let relation_helpers: Vec<TokenStream> = table
        .relations
        .iter()
        .map(|r| {
            let target = crate::lookup::load_table(&r.target_struct, span)?;
            crate::relation::helper_fn_def(
                &crate::relation::Included {
                    relation: r.clone(),
                    target,
                },
                span,
            )
        })
        .collect::<syn::Result<_>>()?;

    let mut cols: Vec<Col> = Vec::new();
    for (def, field) in table.columns.iter().zip(&column_fields) {
        cols.push(Col {
            def: def.clone(),
            ident: field.ident.clone().expect("named field"),
            inner_ty: syn::parse_str(&def.rust_type)
                .map_err(|e| syn::Error::new(span, format!("{}: {e}", def.rust_type)))?,
            field_ty: field.ty.clone(),
        });
    }

    let ident = &item.ident;
    let table_name = &table.name;
    let quoted_table = fse_schema::sql::quote_ident(table_name);
    let pk_cols: Vec<&Col> = cols.iter().filter(|c| c.def.primary_key).collect();
    let non_pk_cols: Vec<&Col> = cols.iter().filter(|c| !c.def.primary_key).collect();

    let select_list = crate::codegen::select_list(&table.columns);
    let build_fields = crate::codegen::build_fields(&table.columns);

    let pk_params: Vec<TokenStream> = pk_cols.iter().map(|c| fn_param(c)).collect();
    let (pk_bind_locals, pk_bind_names) =
        hoist_binds(pk_cols.iter().map(|c| param_bind(c)).collect());
    let pk_where = pk_cols
        .iter()
        .map(|c| format!("{} = ?", fse_schema::sql::quote_ident(&c.def.name)))
        .collect::<Vec<_>>()
        .join(" AND ");

    let fetch_sql = format!("SELECT {select_list} FROM {quoted_table} WHERE {pk_where}");
    let fetch_all_sql = format!("SELECT {select_list} FROM {quoted_table}");
    let count_sql = format!("SELECT COUNT(*) as \"count!: i64\" FROM {quoted_table}");
    let delete_sql = format!("DELETE FROM {quoted_table} WHERE {pk_where}");

    let mut methods = vec![quote! {
        pub async fn fetch(
            db: impl ::sqlx::SqliteExecutor<'_>,
            #(#pk_params),*
        ) -> ::sqlx::Result<Option<Self>> {
            #(#pk_bind_locals)*
            let row = ::sqlx::query!(#fetch_sql, #(#pk_bind_names),*).fetch_optional(db).await?;
            match row {
                Some(r) => Ok(Some(Self { #(#build_fields,)* #(#relation_inits),* })),
                None => Ok(None),
            }
        }

        pub async fn fetch_all(db: impl ::sqlx::SqliteExecutor<'_>) -> ::sqlx::Result<Vec<Self>> {
            let rows = ::sqlx::query!(#fetch_all_sql).fetch_all(db).await?;
            rows.into_iter()
                .map(|r| -> ::sqlx::Result<Self> { Ok(Self { #(#build_fields,)* #(#relation_inits),* }) })
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
        let sql = format!(
            "SELECT {select_list} FROM {quoted_table} WHERE {} = ?",
            fse_schema::sql::quote_ident(&c.def.name)
        );
        methods.push(quote! {
            pub async fn #method(
                db: impl ::sqlx::SqliteExecutor<'_>,
                #param
            ) -> ::sqlx::Result<Option<Self>> {
                #(#bind_local)*
                let row = ::sqlx::query!(#sql, #(#bind_name),*).fetch_optional(db).await?;
                match row {
                    Some(r) => Ok(Some(Self { #(#build_fields,)* #(#relation_inits),* })),
                    None => Ok(None),
                }
            }
        });
    }

    // Full-row UPDATE by pk (skipped for pk-only tables, e.g. join tables).
    if !non_pk_cols.is_empty() {
        let set_list = non_pk_cols
            .iter()
            .map(|c| format!("{} = ?", fse_schema::sql::quote_ident(&c.def.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("UPDATE {quoted_table} SET {set_list} WHERE {pk_where}");
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

    let col_consts = column_consts(&cols)?;
    let from_row_fields: Vec<TokenStream> = cols.iter().map(from_row_field).collect();

    Ok(quote! {
        impl #ident {
            pub const TABLE: &'static str = #table_name;

            #(#col_consts)*

            /// Dynamic (unchecked) SELECT — for query shapes decided at
            /// runtime. Prefer the checked `find!` macro where possible.
            pub fn find() -> ::fse_orm::SelectBuilder<Self> {
                ::fse_orm::SelectBuilder::new(Self::TABLE)
            }

            /// Dynamic (unchecked) partial UPDATE.
            pub fn update_set() -> ::fse_orm::UpdateBuilder {
                ::fse_orm::UpdateBuilder::new(Self::TABLE)
            }

            /// Dynamic (unchecked) DELETE.
            pub fn delete_where() -> ::fse_orm::DeleteBuilder {
                ::fse_orm::DeleteBuilder::new(Self::TABLE)
            }

            #(#methods)*

            #(#relation_helpers)*
        }

        impl<'r> ::sqlx::FromRow<'r, ::sqlx::sqlite::SqliteRow> for #ident {
            fn from_row(row: &'r ::sqlx::sqlite::SqliteRow) -> ::std::result::Result<Self, ::sqlx::Error> {
                Ok(Self { #(#from_row_fields,)* #(#relation_inits),* })
            }
        }
    })
}

/// Typed column tokens for the dynamic builder: `Product::STATUS`,
/// `Product::CREATED_AT`, ... json columns get none (their TEXT form is not
/// meaningfully comparable).
fn column_consts(cols: &[Col]) -> syn::Result<Vec<TokenStream>> {
    let mut consts = Vec::new();
    for c in cols.iter().filter(|c| !c.def.json) {
        let name = &c.def.name;
        let upper = name.to_uppercase();
        if upper == "TABLE" {
            return Err(syn::Error::new(
                c.ident.span(),
                "a column named `table` collides with the generated TABLE const; rename it",
            ));
        }
        let const_ident = format_ident!("{upper}");
        let ty = &c.inner_ty;
        consts.push(quote! {
            pub const #const_ident: ::fse_orm::Col<#ty> = ::fse_orm::Col::new(#name);
        });
    }
    Ok(consts)
}

/// One field of the generated `FromRow` impl, with the same json/enum
/// conversions as the checked queries.
fn from_row_field(c: &Col) -> TokenStream {
    let id = &c.ident;
    let name = &c.def.name;
    if c.def.json {
        if c.def.nullable {
            quote! {
                #id: ::fse_orm::opt_from_json_str(
                    #name,
                    ::sqlx::Row::try_get::<::std::option::Option<::std::string::String>, _>(row, #name)?.as_deref(),
                )?
            }
        } else {
            quote! {
                #id: ::fse_orm::from_json_str(
                    #name,
                    &::sqlx::Row::try_get::<::std::string::String, _>(row, #name)?,
                )?
            }
        }
    } else if c.def.is_enum {
        if c.def.nullable {
            quote! {
                #id: ::fse_orm::opt_parse_db_value(
                    #name,
                    ::sqlx::Row::try_get::<::std::option::Option<::std::string::String>, _>(row, #name)?.as_deref(),
                )?
            }
        } else {
            quote! {
                #id: ::fse_orm::parse_db_value(
                    #name,
                    &::sqlx::Row::try_get::<::std::string::String, _>(row, #name)?,
                )?
            }
        }
    } else {
        let ty = &c.field_ty;
        quote! { #id: ::sqlx::Row::try_get::<#ty, _>(row, #name)? }
    }
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
