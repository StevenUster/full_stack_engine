//! Emission of the per-model `ModelResource` implementation — the typed data
//! access behind generated CRUD.
//!
//! Everything here expands to *typed* ORM calls monomorphized for the one
//! table: the checked `insert!`/`update!` macros (compile-time verified SQL)
//! and the `Col`-token dynamic builder for the runtime-shaped list query.
//! Form values are parsed by the framework's `models::form` helpers into the
//! column's Rust type before anything touches the database.

use fse_schema::{SqlType, TableDef};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::model::{ColInfo, ModelOpts};

pub fn emit(
    ident: &syn::Ident,
    table: &TableDef,
    opts: &ModelOpts,
    cols: &[ColInfo],
) -> TokenStream {
    let row_json = emit_row_json(ident, cols);
    let list = emit_list(ident, table, cols);
    let get = emit_get(ident);
    let get_by_public = emit_get_by_public(ident, opts, cols);
    let create = emit_write(ident, cols, Write::Create);
    let update = emit_write(ident, cols, Write::Update);
    let delete = emit_delete(ident);

    quote! {
        #row_json

        struct __FseModelResource;
        static __FSE_MODEL_RESOURCE: __FseModelResource = __FseModelResource;

        impl ::full_stack_engine::models::ModelResource for __FseModelResource {
            #list
            #get
            #get_by_public
            #create
            #update
            #delete
        }
    }
}

/// The `Col` token const the Table derive generates for a column.
fn col_const(name: &str) -> syn::Ident {
    format_ident!("{}", name.to_uppercase())
}

fn field_ident(name: &str) -> syn::Ident {
    format_ident!("{}", name)
}

/// `fn __fse_row_json(&Row) -> Value` over the visible columns (plus the
/// primary key, which links/deletes always need). Enum values serialize as
/// their stored string via `as_str()` — no `Serialize` bound on the enum.
fn emit_row_json(ident: &syn::Ident, cols: &[ColInfo]) -> TokenStream {
    let pairs = cols
        .iter()
        .filter(|c| !c.hidden || c.def.primary_key)
        .map(|c| {
            let key = &c.def.name;
            let fid = field_ident(&c.def.name);
            let value = if c.def.is_enum {
                if c.def.nullable {
                    quote!(__row.#fid.as_ref().map(|v| v.as_str()))
                } else {
                    quote!(__row.#fid.as_str())
                }
            } else {
                quote!(&__row.#fid)
            };
            quote!(#key: #value)
        });

    quote! {
        fn __fse_row_json(__row: &#ident) -> ::full_stack_engine::models::serde_json::Value {
            ::full_stack_engine::models::serde_json::json!({ #(#pairs),* })
        }
    }
}

fn emit_list(ident: &syn::Ident, table: &TableDef, cols: &[ColInfo]) -> TokenStream {
    let search_cols: Vec<syn::Ident> = cols
        .iter()
        .filter(|c| c.ui.search)
        .map(|c| col_const(&c.def.name))
        .collect();
    let search = if search_cols.is_empty() {
        quote!()
    } else {
        let first = &search_cols[0];
        let rest = &search_cols[1..];
        quote! {
            if let ::core::option::Option::Some(__s) = q.search.as_deref() {
                if !__s.is_empty() {
                    #[allow(unused_mut)]
                    let mut __sc = #ident::#first.contains(__s);
                    #( __sc = __sc.or(#ident::#rest.contains(__s)); )*
                    __cond = ::full_stack_engine::models::and_opt(__cond, __sc);
                }
            }
        }
    };

    let filter_arms: Vec<TokenStream> = cols
        .iter()
        .filter(|c| c.ui.filter)
        .map(|c| {
            let name = &c.def.name;
            let cid = col_const(&c.def.name);
            if c.def.ty == SqlType::Boolean {
                quote! {
                    #name => {
                        match __value.as_str() {
                            "true" | "1" => {
                                __cond = ::full_stack_engine::models::and_opt(
                                    __cond,
                                    #ident::#cid.eq(true),
                                );
                            }
                            "false" | "0" => {
                                __cond = ::full_stack_engine::models::and_opt(
                                    __cond,
                                    #ident::#cid.eq(false),
                                );
                            }
                            _ => {}
                        }
                    }
                }
            } else {
                let ty = &c.inner_ty;
                quote! {
                    #name => {
                        if let ::core::result::Result::Ok(__v) = __value.parse::<#ty>() {
                            __cond = ::full_stack_engine::models::and_opt(
                                __cond,
                                #ident::#cid.eq(__v),
                            );
                        }
                    }
                }
            }
        })
        .collect();
    let filters = if filter_arms.is_empty() {
        quote!()
    } else {
        quote! {
            for (__name, __value) in &q.filters {
                if __value.is_empty() {
                    continue;
                }
                match __name.as_str() {
                    #(#filter_arms)*
                    _ => {}
                }
            }
        }
    };

    let sort_arms = cols.iter().filter(|c| !c.def.json).map(|c| {
        let name = &c.def.name;
        let cid = col_const(&c.def.name);
        quote! {
            ::core::option::Option::Some(#name) => {
                if q.desc { #ident::#cid.desc() } else { #ident::#cid.asc() }
            }
        }
    });
    let default_order = if cols
        .iter()
        .any(|c| c.def.name == "created_at" && !c.def.json)
    {
        let cid = col_const("created_at");
        quote!(#ident::#cid.desc())
    } else {
        let cid = col_const(&table.primary_key()[0].name);
        quote!(#ident::#cid.desc())
    };

    quote! {
        fn list<'a>(
            &'a self,
            db: &'a ::full_stack_engine::models::Db,
            q: &'a ::full_stack_engine::models::ListQuery,
        ) -> ::full_stack_engine::models::BoxFuture<
            'a,
            ::full_stack_engine::models::DbResult<::full_stack_engine::models::ListResult>,
        > {
            ::std::boxed::Box::pin(async move {
                #[allow(unused_mut)]
                let mut __cond: ::core::option::Option<::fse_orm::Cond> =
                    ::core::option::Option::None;
                #search
                #filters
                let mut __sel = #ident::find();
                if let ::core::option::Option::Some(__c) = __cond {
                    __sel = __sel.filter(__c);
                }
                let __order = match q.sort.as_deref() {
                    #(#sort_arms)*
                    _ => #default_order,
                };
                let __page = __sel.order_by(__order).fetch_page(db, q.page, q.per_page).await?;
                ::core::result::Result::Ok(::full_stack_engine::models::ListResult {
                    rows: __page.rows.iter().map(__fse_row_json).collect(),
                    total: __page.total,
                    page: q.page.max(1),
                    per_page: q.per_page.max(1),
                })
            })
        }
    }
}

fn emit_get(ident: &syn::Ident) -> TokenStream {
    quote! {
        fn get<'a>(
            &'a self,
            db: &'a ::full_stack_engine::models::Db,
            id: i64,
        ) -> ::full_stack_engine::models::BoxFuture<
            'a,
            ::full_stack_engine::models::DbResult<
                ::core::option::Option<::full_stack_engine::models::serde_json::Value>,
            >,
        > {
            ::std::boxed::Box::pin(async move {
                ::core::result::Result::Ok(
                    #ident::fetch(db, id).await?.as_ref().map(__fse_row_json),
                )
            })
        }
    }
}

fn emit_get_by_public(ident: &syn::Ident, opts: &ModelOpts, cols: &[ColInfo]) -> TokenStream {
    let signature = quote! {
        fn get_by_public<'a>(
            &'a self,
            db: &'a ::full_stack_engine::models::Db,
            __key: &'a str,
        ) -> ::full_stack_engine::models::BoxFuture<
            'a,
            ::full_stack_engine::models::DbResult<
                ::core::option::Option<::full_stack_engine::models::serde_json::Value>,
            >,
        >
    };

    let Some(public) = &opts.public_read else {
        return quote! {
            #signature {
                let _ = (db, __key);
                ::std::boxed::Box::pin(async move { ::core::result::Result::Ok(::core::option::Option::None) })
            }
        };
    };

    let col = cols
        .iter()
        .find(|c| &c.def.name == public)
        .expect("public_read validated against the columns");
    let cid = col_const(&col.def.name);
    let fetch = quote! {
        ::core::result::Result::Ok(
            #ident::find()
                .filter(#ident::#cid.eq(__k))
                .fetch_optional(db)
                .await?
                .as_ref()
                .map(__fse_row_json),
        )
    };
    let body = if col.def.rust_type == "String" {
        quote! {
            let __k = __key.to_string();
            #fetch
        }
    } else {
        let ty = &col.inner_ty;
        quote! {
            let ::core::result::Result::Ok(__k) = __key.parse::<#ty>() else {
                return ::core::result::Result::Ok(::core::option::Option::None);
            };
            #fetch
        }
    };

    quote! {
        #signature {
            ::std::boxed::Box::pin(async move { #body })
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Write {
    Create,
    Update,
}

/// The shared create/update shape: parse every form field (collecting all
/// errors), pre-check unique columns, then run the checked `insert!` /
/// `update!` with the typed values.
fn emit_write(ident: &syn::Ident, cols: &[ColInfo], kind: Write) -> TokenStream {
    let form_cols: Vec<&ColInfo> = cols.iter().filter(|c| c.in_form).collect();

    let signature = match kind {
        Write::Create => quote! {
            fn create<'a>(
                &'a self,
                db: &'a ::full_stack_engine::models::Db,
                __form: &'a ::full_stack_engine::models::FormData,
            ) -> ::full_stack_engine::models::BoxFuture<
                'a,
                ::full_stack_engine::models::DbResult<
                    ::core::result::Result<i64, ::full_stack_engine::models::FormErrors>,
                >,
            >
        },
        Write::Update => quote! {
            fn update<'a>(
                &'a self,
                db: &'a ::full_stack_engine::models::Db,
                __id: i64,
                __form: &'a ::full_stack_engine::models::FormData,
            ) -> ::full_stack_engine::models::BoxFuture<
                'a,
                ::full_stack_engine::models::DbResult<
                    ::core::result::Result<(), ::full_stack_engine::models::FormErrors>,
                >,
            >
        },
    };

    if form_cols.is_empty() {
        // A table with no editable columns (e.g. only id + defaults) has no
        // meaningful generated form.
        let body = quote! {
            ::std::boxed::Box::pin(async move {
                ::core::result::Result::Ok(::core::result::Result::Err(::std::vec![
                    ::full_stack_engine::models::FieldError {
                        field: "",
                        code: "not_supported",
                    }
                ]))
            })
        };
        return quote!(#signature { let _ = (db, __form); #body });
    }

    let parse_stmts: Vec<TokenStream> = form_cols.iter().map(|c| parse_stmt(c)).collect();
    let unique_checks: Vec<TokenStream> = form_cols
        .iter()
        .filter(|c| c.def.unique && !c.orm_text && !c.def.json)
        .map(|c| unique_check(ident, c, kind))
        .collect();

    let assigns = form_cols.iter().map(|c| {
        let fid = field_ident(&c.def.name);
        let var = format_ident!("__v_{}", c.def.name);
        quote!(#fid = #var.unwrap())
    });

    let tail = match kind {
        Write::Create => quote! {
            let __row = ::fse_orm::insert!(#ident, db, #(#assigns),*).await?;
            ::core::result::Result::Ok(::core::result::Result::Ok(__row.id))
        },
        Write::Update => quote! {
            ::fse_orm::update!(#ident, db, id == __id; #(#assigns),*).await?;
            ::core::result::Result::Ok(::core::result::Result::Ok(()))
        },
    };

    quote! {
        #signature {
            ::std::boxed::Box::pin(async move {
                let mut __errors: ::full_stack_engine::models::FormErrors = ::std::vec::Vec::new();
                #(#parse_stmts)*
                #(#unique_checks)*
                if !__errors.is_empty() {
                    return ::core::result::Result::Ok(::core::result::Result::Err(__errors));
                }
                #tail
            })
        }
    }
}

/// `let __v_<col> = <form helper>(...)` — every variable is an `Option`
/// whose `None` means "invalid, error recorded".
fn parse_stmt(c: &ColInfo) -> TokenStream {
    let name = &c.def.name;
    let var = format_ident!("__v_{}", c.def.name);
    let form = quote!(::full_stack_engine::models::form);
    let nullable = c.def.nullable;
    let ty = &c.inner_ty;

    let expr = if c.def.is_enum {
        if nullable {
            quote!(#form::opt_parse::<#ty>(__form, #name, "invalid_option", &mut __errors))
        } else {
            quote!(#form::req_parse::<#ty>(__form, #name, "invalid_option", &mut __errors))
        }
    } else {
        match c.def.ty {
            SqlType::Boolean => {
                let checked = quote!(#form::checkbox(__form, #name));
                if nullable {
                    quote!(::core::option::Option::Some(::core::option::Option::Some(#checked)))
                } else {
                    quote!(::core::option::Option::Some(#checked))
                }
            }
            SqlType::Integer | SqlType::Real => {
                if nullable {
                    quote!(#form::opt_parse::<#ty>(__form, #name, "invalid_number", &mut __errors))
                } else {
                    quote!(#form::req_parse::<#ty>(__form, #name, "invalid_number", &mut __errors))
                }
            }
            SqlType::Timestamp => {
                if nullable {
                    quote!(#form::opt_datetime(__form, #name, &mut __errors))
                } else {
                    quote!(#form::req_datetime(__form, #name, &mut __errors))
                }
            }
            SqlType::Text if c.def.rust_type == "String" => {
                if nullable {
                    quote!(::core::option::Option::Some(#form::opt_str(__form, #name)))
                } else {
                    quote!(#form::req_str(__form, #name, &mut __errors))
                }
            }
            // Non-String TEXT natives (NaiveDate, NaiveTime, Uuid): FromStr.
            SqlType::Text => {
                if nullable {
                    quote!(#form::opt_parse::<#ty>(__form, #name, "invalid_value", &mut __errors))
                } else {
                    quote!(#form::req_parse::<#ty>(__form, #name, "invalid_value", &mut __errors))
                }
            }
            SqlType::Blob => unreachable!("blob columns are never in_form"),
        }
    };
    quote!(let #var = #expr;)
}

/// Pre-check a unique column so a duplicate becomes a `not_unique` field
/// error on the re-rendered form instead of a raw constraint violation.
/// Updates exclude the row being edited.
fn unique_check(ident: &syn::Ident, c: &ColInfo, kind: Write) -> TokenStream {
    let name = &c.def.name;
    let var = format_ident!("__v_{}", c.def.name);
    let cid = col_const(&c.def.name);
    let push = quote! {
        __errors.push(::full_stack_engine::models::FieldError {
            field: #name,
            code: "not_unique",
        });
    };
    let guard = match kind {
        Write::Create => push,
        Write::Update => quote!(if __existing.id != __id { #push }),
    };
    let pattern = if c.def.nullable {
        quote!(::core::option::Option::Some(::core::option::Option::Some(__u)))
    } else {
        quote!(::core::option::Option::Some(__u))
    };
    quote! {
        if let #pattern = &#var {
            if let ::core::option::Option::Some(__existing) = #ident::find()
                .filter(#ident::#cid.eq(__u.clone()))
                .fetch_optional(db)
                .await?
            {
                #guard
            }
        }
    }
}

fn emit_delete(ident: &syn::Ident) -> TokenStream {
    quote! {
        fn delete<'a>(
            &'a self,
            db: &'a ::full_stack_engine::models::Db,
            id: i64,
        ) -> ::full_stack_engine::models::BoxFuture<
            'a,
            ::full_stack_engine::models::DbResult<u64>,
        > {
            ::std::boxed::Box::pin(#ident::delete(db, id))
        }
    }
}
