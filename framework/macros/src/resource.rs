//! Emission of the per-model `ModelResource` implementation — the typed data
//! access behind generated CRUD.
//!
//! Everything here expands to *typed* ORM calls monomorphized for the one
//! table: the checked `insert!`/`update!` macros (compile-time verified SQL)
//! and the `Col`-token dynamic builder for the runtime-shaped list query.
//! Form values are parsed by the framework's `models::form` helpers into the
//! column's Rust type before anything touches the database.
//!
//! Every read and write goes through `__fse_scope`/`__fse_fetch`, which
//! apply the model's access rules (owner, `ModelHooks::scope`/`public_scope`,
//! the parent filter) — there is no code path around them.

use fse_schema::{DefaultValue, SqlType, TableDef};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::model::{ColInfo, FilterKind, ModelOpts};

pub fn emit(
    ident: &syn::Ident,
    table: &TableDef,
    opts: &ModelOpts,
    cols: &[ColInfo],
    creatable: bool,
    title_field: &str,
) -> TokenStream {
    let row_json = emit_row_json(ident, cols);
    let scope = emit_scope(ident, cols);
    let allowed = emit_allowed_actions(ident, opts);
    let list = emit_list(ident, table, opts, cols);
    let get = emit_get(ident);
    let get_by_public = emit_get_by_public(ident, opts, cols);
    let refs = emit_refs(ident, table, cols, title_field);
    let create = emit_write(ident, cols, Write::Create, creatable);
    let update = emit_write(ident, cols, Write::Update, true);
    let delete = emit_delete(ident);
    let act = emit_act(ident, opts);
    let models = quote!(::full_stack_engine::models);
    let hooks = quote!(<#ident as #models::ModelHooks>);

    quote! {
        #row_json
        #scope
        #allowed

        struct __FseModelResource;
        static __FSE_MODEL_RESOURCE: __FseModelResource = __FseModelResource;

        impl #models::ModelResource for __FseModelResource {
            #list
            #get
            #get_by_public
            #refs

            fn visible_ids<'a>(
                &'a self,
                db: &'a #models::Db,
                __access: #models::Access<'a>,
            ) -> #models::BoxFuture<
                'a,
                #models::AppResult<::core::option::Option<::std::vec::Vec<i64>>>,
            > {
                ::std::boxed::Box::pin(__fse_visible_ids(db, __access))
            }

            fn in_nav(&self, __user: &#models::CurrentUser) -> bool {
                #hooks::in_nav(__user)
            }

            fn can_create<'a>(
                &'a self,
                db: &'a #models::Db,
                __access: #models::Access<'a>,
            ) -> #models::BoxFuture<'a, #models::AppResult<bool>> {
                ::std::boxed::Box::pin(async move {
                    match __access.user {
                        ::core::option::Option::Some(__user) => {
                            #hooks::can_create(db, __user, __access.parent_id).await
                        }
                        ::core::option::Option::None => ::core::result::Result::Ok(false),
                    }
                })
            }

            #create
            #update
            #delete
            #act
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

/// The JSON value of column `c` on `__row` — enums as their stored string.
fn json_value(c: &ColInfo, row: &TokenStream) -> TokenStream {
    let fid = field_ident(&c.def.name);
    if c.def.is_enum {
        if c.def.nullable {
            quote!(#row.#fid.as_ref().map(|v| v.as_str()))
        } else {
            quote!(#row.#fid.as_str())
        }
    } else {
        quote!(&#row.#fid)
    }
}

/// `fn __fse_row_json(&Row) -> Value` over the visible columns (plus the
/// primary key, which links/deletes always need), then the model's
/// `decorate` hook. Hidden and secret columns never reach a page or an API
/// response.
fn emit_row_json(ident: &syn::Ident, cols: &[ColInfo]) -> TokenStream {
    let row = quote!(__row);
    let visible: Vec<&ColInfo> = cols
        .iter()
        .filter(|c| !c.hidden || c.def.primary_key)
        .collect();
    let pairs = visible.iter().filter(|c| !c.ui.private).map(|c| {
        let key = &c.def.name;
        let value = json_value(c, &row);
        quote!(#key: #value)
    });
    // `#[ui(private)]` columns: signed-in pages only, never anonymous ones.
    let private = visible.iter().filter(|c| c.ui.private).map(|c| {
        let key = &c.def.name;
        let value = json_value(c, &row);
        quote! {
            if !__public {
                __m.insert(#key.into(), ::full_stack_engine::models::serde_json::json!(#value));
            }
        }
    });

    quote! {
        fn __fse_row_json(
            __row: &#ident,
            __public: bool,
        ) -> ::full_stack_engine::models::serde_json::Value {
            let mut __v = ::full_stack_engine::models::serde_json::json!({ #(#pairs),* });
            if let ::full_stack_engine::models::serde_json::Value::Object(__m) = &mut __v {
                #(#private)*
                let _ = __public;
                <#ident as ::full_stack_engine::models::ModelHooks>::decorate(__row, __m);
            }
            __v
        }
    }
}

/// The access rules every read and write goes through:
/// `__fse_scope` combines the `owner` column, the model's
/// `ModelHooks::scope` (or `public_scope` for anonymous access) and the
/// parent filter, and `__fse_fetch` loads one row by id inside that scope.
fn emit_scope(ident: &syn::Ident, cols: &[ColInfo]) -> TokenStream {
    let models = quote!(::full_stack_engine::models);
    let hooks = quote!(<#ident as #models::ModelHooks>);
    let owner = cols.iter().find(|c| c.is_owner).map(|c| {
        let cid = col_const(&c.def.name);
        quote! {
            if !__user.is_admin() {
                __cond = #models::and_opt(__cond, #ident::#cid.eq(__user.id()));
            }
        }
    });
    // A nested row is visible only under a parent the user can see — on
    // every path, not just under the parent's URL.
    let parent = cols.iter().find(|c| c.is_parent).map(|c| {
        let cid = col_const(&c.def.name);
        let target = c.references().expect("parent is a foreign key");
        quote! {
            if let ::core::option::Option::Some(__user) = __access.user {
                if let ::core::option::Option::Some(__ids) =
                    #models::visible_parent_ids(db, __user, #target).await?
                {
                    __cond = #models::and_opt(__cond, #ident::#cid.in_(__ids));
                }
            }
            if let ::core::option::Option::Some(__pid) = __access.parent_id {
                __cond = #models::and_opt(__cond, #ident::#cid.eq(__pid));
            }
        }
    });

    quote! {
        async fn __fse_scope(
            db: &#models::Db,
            __access: #models::Access<'_>,
        ) -> #models::AppResult<::core::option::Option<#models::Cond>> {
            #[allow(unused_mut)]
            let mut __cond = match __access.user {
                ::core::option::Option::None => #hooks::public_scope(),
                ::core::option::Option::Some(__user) => {
                    #[allow(unused_mut)]
                    let mut __cond = #hooks::scope(db, __user).await?;
                    #owner
                    __cond
                }
            };
            #parent
            ::core::result::Result::Ok(__cond)
        }

        async fn __fse_fetch(
            db: &#models::Db,
            __access: #models::Access<'_>,
            __id: i64,
        ) -> #models::AppResult<::core::option::Option<#ident>> {
            let mut __sel = #ident::find().filter(#ident::ID.eq(__id));
            if let ::core::option::Option::Some(__c) = __fse_scope(db, __access).await? {
                __sel = __sel.filter(__c);
            }
            ::core::result::Result::Ok(__sel.fetch_optional(db).await?)
        }

        async fn __fse_visible_ids(
            db: &#models::Db,
            __access: #models::Access<'_>,
        ) -> #models::AppResult<::core::option::Option<::std::vec::Vec<i64>>> {
            let ::core::option::Option::Some(__c) = __fse_scope(db, __access).await? else {
                return ::core::result::Result::Ok(::core::option::Option::None);
            };
            ::core::result::Result::Ok(::core::option::Option::Some(
                #ident::find()
                    .filter(__c)
                    .fetch_all(db)
                    .await?
                    .iter()
                    .map(|__r| __r.id)
                    .collect(),
            ))
        }

        fn __fse_not_found() -> #models::AppError {
            #models::AppError::NotFound(::std::format!(
                "{} row not found",
                ::core::stringify!(#ident)
            ))
        }
    }
}

/// `__fse_allowed_actions(row, db, user)` — the row actions `can_act` allows.
fn emit_allowed_actions(ident: &syn::Ident, opts: &ModelOpts) -> TokenStream {
    let models = quote!(::full_stack_engine::models);
    let hooks = quote!(<#ident as #models::ModelHooks>);
    let checks = opts.actions.iter().map(|a| {
        let name = a.to_string();
        quote! {
            if #hooks::can_act(__row, #name, db, __user).await? {
                __allowed.push(#name);
            }
        }
    });
    let unused = if opts.actions.is_empty() {
        quote!(let _ = (__row, db, __user);)
    } else {
        quote!()
    };
    quote! {
        async fn __fse_allowed_actions(
            __row: &#ident,
            db: &#models::Db,
            __user: &#models::CurrentUser,
        ) -> #models::AppResult<::std::vec::Vec<&'static str>> {
            #unused
            #[allow(unused_mut)]
            let mut __allowed: ::std::vec::Vec<&'static str> = ::std::vec::Vec::new();
            #(#checks)*
            ::core::result::Result::Ok(__allowed)
        }
    }
}

/// The `match` arms (keyed by query param) one filter column contributes.
fn filter_arms(ident: &syn::Ident, c: &ColInfo, kind: FilterKind) -> TokenStream {
    let name = &c.def.name;
    let cid = col_const(&c.def.name);
    let ty = &c.inner_ty;
    let and = quote!(::full_stack_engine::models::and_opt);
    // A private column's filter does nothing for anonymous requests: its
    // arms only match when signed in, else `_ => {}` swallows the param.
    let guard = if c.ui.private {
        quote!(if !__public)
    } else {
        quote!()
    };
    // `__value` parsed into the column type; unparsable input is ignored,
    // like an unknown filter. Expands to the `pattern = expr` of an
    // `if let` binding `__v`.
    let parsed = |bound_end: bool| {
        if c.def.ty == SqlType::Timestamp {
            quote! {
                ::core::option::Option::Some(__v) =
                    ::full_stack_engine::models::form::filter_datetime(__value, #bound_end)
            }
        } else {
            quote!(::core::result::Result::Ok(__v) = __value.parse::<#ty>())
        }
    };
    match kind {
        FilterKind::Exact if c.def.ty == SqlType::Boolean => quote! {
            #name #guard => match __value.as_str() {
                "true" | "1" => __cond = #and(__cond, #ident::#cid.eq(true)),
                "false" | "0" => __cond = #and(__cond, #ident::#cid.eq(false)),
                _ => {}
            },
        },
        FilterKind::Exact => {
            let value = parsed(false);
            quote! {
                #name #guard => {
                    if let #value {
                        __cond = #and(__cond, #ident::#cid.eq(__v));
                    }
                }
            }
        }
        FilterKind::Contains => quote! {
            #name #guard => __cond = #and(__cond, #ident::#cid.contains(__value.as_str())),
        },
        FilterKind::Range => {
            let from = format!("{name}_from");
            let to = format!("{name}_to");
            let (lower, upper) = (parsed(false), parsed(true));
            quote! {
                #from #guard => {
                    if let #lower {
                        __cond = #and(__cond, #ident::#cid.gte(__v));
                    }
                }
                #to #guard => {
                    if let #upper {
                        __cond = #and(__cond, #ident::#cid.lte(__v));
                    }
                }
            }
        }
    }
}

fn emit_list(
    ident: &syn::Ident,
    table: &TableDef,
    opts: &ModelOpts,
    cols: &[ColInfo],
) -> TokenStream {
    let models = quote!(::full_stack_engine::models);
    // Anonymous requests can't search, filter or sort by a `private`
    // column — even indirectly, a match would reveal its contents.
    let search_terms: Vec<TokenStream> = cols
        .iter()
        .filter(|c| c.ui.search)
        .map(|c| {
            let cid = col_const(&c.def.name);
            let term = quote! {
                __sc = ::core::option::Option::Some(match __sc {
                    ::core::option::Option::Some(__prev) => __prev.or(#ident::#cid.contains(__s)),
                    ::core::option::Option::None => #ident::#cid.contains(__s),
                });
            };
            if c.ui.private {
                quote!(if !__public { #term })
            } else {
                term
            }
        })
        .collect();
    let search = if search_terms.is_empty() {
        quote!()
    } else {
        quote! {
            if let ::core::option::Option::Some(__s) = q.search.as_deref() {
                if !__s.is_empty() {
                    let mut __sc: ::core::option::Option<#models::Cond> =
                        ::core::option::Option::None;
                    #(#search_terms)*
                    if let ::core::option::Option::Some(__sc) = __sc {
                        __cond = #models::and_opt(__cond, __sc);
                    }
                }
            }
        }
    };

    let filter_arms: Vec<TokenStream> = cols
        .iter()
        .filter_map(|c| c.ui.filter.map(|kind| filter_arms(ident, c, kind)))
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

    // Only visible columns are sortable: ordering by a hidden column (a
    // password hash, a token) would leak information about it.
    let sort_arms = cols.iter().filter(|c| !c.def.json && !c.hidden).map(|c| {
        let name = &c.def.name;
        let cid = col_const(&c.def.name);
        let guard = if c.ui.private {
            quote!(if !__public)
        } else {
            quote!()
        };
        quote! {
            ::core::option::Option::Some(#name) #guard => {
                if q.desc { #ident::#cid.desc() } else { #ident::#cid.asc() }
            }
        }
    });
    let order_tokens = |col: &str, desc: bool| {
        let cid = col_const(col);
        if desc {
            quote!(#ident::#cid.desc())
        } else {
            quote!(#ident::#cid.asc())
        }
    };
    let default_order = if let Some((col, desc)) = &opts.order_by {
        order_tokens(col, *desc)
    } else if cols
        .iter()
        .any(|c| c.def.name == "created_at" && !c.def.json)
    {
        let cid = col_const("created_at");
        quote!(#ident::#cid.desc())
    } else {
        let cid = col_const(&table.primary_key()[0].name);
        quote!(#ident::#cid.desc())
    };
    // `public_order_by` only changes the public list's default; an explicit
    // `?sort=` still wins on both.
    let default_order = match &opts.public_order_by {
        Some((col, desc)) => {
            let public_order = order_tokens(col, *desc);
            quote!(if __public { #public_order } else { #default_order })
        }
        None => default_order,
    };

    let row_actions = if opts.actions.is_empty() {
        quote!()
    } else {
        quote! {
            if let ::core::option::Option::Some(__u) = __access.user {
                __j["_actions"] = #models::serde_json::json!(
                    __fse_allowed_actions(__r, db, __u).await?
                );
            }
        }
    };

    quote! {
        fn list<'a>(
            &'a self,
            db: &'a #models::Db,
            __access: #models::Access<'a>,
            q: &'a #models::ListQuery,
        ) -> #models::BoxFuture<'a, #models::AppResult<#models::ListResult>> {
            ::std::boxed::Box::pin(async move {
                #[allow(unused_variables)]
                let __public = __access.user.is_none();
                #[allow(unused_mut)]
                let mut __cond: ::core::option::Option<#models::Cond> =
                    __fse_scope(db, __access).await?;
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
                let mut __rows = ::std::vec::Vec::with_capacity(__page.rows.len());
                for __r in &__page.rows {
                    #[allow(unused_mut)]
                    let mut __j = __fse_row_json(__r, __access.user.is_none());
                    #row_actions
                    __rows.push(__j);
                }
                ::core::result::Result::Ok(#models::ListResult {
                    rows: __rows,
                    total: __page.total,
                    page: q.page.max(1),
                    per_page: q.per_page.max(1),
                })
            })
        }
    }
}

fn emit_get(ident: &syn::Ident) -> TokenStream {
    let models = quote!(::full_stack_engine::models);
    let hooks = quote!(<#ident as #models::ModelHooks>);
    quote! {
        fn get<'a>(
            &'a self,
            db: &'a #models::Db,
            __access: #models::Access<'a>,
            id: i64,
        ) -> #models::BoxFuture<
            'a,
            #models::AppResult<::core::option::Option<#models::RowView>>,
        > {
            ::std::boxed::Box::pin(async move {
                let ::core::option::Option::Some(__row) = __fse_fetch(db, __access, id).await?
                else {
                    return ::core::result::Result::Ok(::core::option::Option::None);
                };
                let (can_edit, can_delete, actions) = match __access.user {
                    ::core::option::Option::None => (false, false, ::std::vec::Vec::new()),
                    ::core::option::Option::Some(__user) => (
                        #hooks::can_edit(&__row, db, __user).await?,
                        #hooks::can_delete(&__row, db, __user).await?,
                        __fse_allowed_actions(&__row, db, __user).await?,
                    ),
                };
                ::core::result::Result::Ok(::core::option::Option::Some(#models::RowView {
                    row: __fse_row_json(&__row, __access.user.is_none()),
                    can_edit,
                    can_delete,
                    actions,
                }))
            })
        }
    }
}

fn emit_get_by_public(ident: &syn::Ident, opts: &ModelOpts, cols: &[ColInfo]) -> TokenStream {
    let models = quote!(::full_stack_engine::models);
    let signature = quote! {
        fn get_by_public<'a>(
            &'a self,
            db: &'a #models::Db,
            __key: &'a str,
        ) -> #models::BoxFuture<
            'a,
            #models::AppResult<::core::option::Option<#models::serde_json::Value>>,
        >
    };

    let Some(public) = &opts.public_read else {
        return quote! {
            #signature {
                let _ = (db, __key);
                ::std::boxed::Box::pin(async move {
                    ::core::result::Result::Ok(::core::option::Option::None)
                })
            }
        };
    };

    let col = cols
        .iter()
        .find(|c| &c.def.name == public)
        .expect("public_read validated against the columns");
    let cid = col_const(&col.def.name);
    let fetch = quote! {
        let mut __sel = #ident::find().filter(#ident::#cid.eq(__k));
        if let ::core::option::Option::Some(__c) =
            __fse_scope(db, #models::Access::PUBLIC).await?
        {
            __sel = __sel.filter(__c);
        }
        ::core::result::Result::Ok(
            __sel
                .fetch_optional(db)
                .await?
                .as_ref()
                .map(|__r| __fse_row_json(__r, true)),
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

/// `refs`: title + relation keys of the given rows, for embedding them in
/// rows that point at them.
fn emit_refs(ident: &syn::Ident, table: &TableDef, cols: &[ColInfo], title: &str) -> TokenStream {
    let models = quote!(::full_stack_engine::models);
    let row = quote!(__r);
    let title_col = cols
        .iter()
        .find(|c| c.def.name == title)
        .expect("title resolved from the columns");
    let title_value = json_value(title_col, &row);
    let links = table.relations.iter().map(|rel| {
        let name = &rel.field;
        let fid = field_ident(&rel.local_column);
        let nullable = cols
            .iter()
            .find(|c| c.def.name == rel.local_column)
            .is_some_and(|c| c.def.nullable);
        if nullable {
            quote!((#name, __r.#fid))
        } else {
            quote!((#name, ::core::option::Option::Some(__r.#fid)))
        }
    });
    quote! {
        fn refs<'a>(
            &'a self,
            db: &'a #models::Db,
            __ids: &'a [i64],
        ) -> #models::BoxFuture<
            'a,
            #models::AppResult<::std::collections::HashMap<i64, #models::Ref>>,
        > {
            ::std::boxed::Box::pin(async move {
                if __ids.is_empty() {
                    return ::core::result::Result::Ok(::std::collections::HashMap::new());
                }
                let __rows = #ident::find()
                    .filter(#ident::ID.in_(__ids.iter().copied()))
                    .fetch_all(db)
                    .await?;
                ::core::result::Result::Ok(
                    __rows
                        .iter()
                        .map(|__r| {
                            (
                                __r.id,
                                #models::Ref {
                                    title: #models::serde_json::json!(#title_value),
                                    links: ::std::vec![#(#links),*],
                                },
                            )
                        })
                        .collect(),
                )
            })
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Write {
    Create,
    Update,
}

/// The shared create/update shape: access checks, the `before_save` hook,
/// `slug_from` filling, parsing every form field (collecting all errors),
/// declarative validation, foreign-key visibility, unique pre-checks — then
/// the checked `insert!` / `update!` with the typed values and `after_save`.
#[allow(clippy::too_many_lines)]
fn emit_write(ident: &syn::Ident, cols: &[ColInfo], kind: Write, possible: bool) -> TokenStream {
    let form_cols: Vec<&ColInfo> = cols.iter().filter(|c| c.in_form).collect();

    let models = quote!(::full_stack_engine::models);
    let hooks = quote!(<#ident as #models::ModelHooks>);
    let signature = match kind {
        Write::Create => quote! {
            fn create<'a>(
                &'a self,
                db: &'a #models::Db,
                __access: #models::Access<'a>,
                __form: &'a #models::FormData,
            ) -> #models::BoxFuture<
                'a,
                #models::AppResult<::core::result::Result<i64, #models::FormErrors>>,
            >
        },
        Write::Update => quote! {
            fn update<'a>(
                &'a self,
                db: &'a #models::Db,
                __access: #models::Access<'a>,
                __id: i64,
                __form: &'a #models::FormData,
            ) -> #models::BoxFuture<
                'a,
                #models::AppResult<::core::result::Result<(), #models::FormErrors>>,
            >
        },
    };

    if form_cols.is_empty() || !possible {
        // No editable columns (e.g. only id + defaults), or a create that
        // couldn't fill every required column: no generated form.
        let body = quote! {
            ::std::boxed::Box::pin(async move {
                ::core::result::Result::Ok(::core::result::Result::Err(::std::vec![
                    #models::FieldError {
                        field: "",
                        code: "not_supported",
                    }
                ]))
            })
        };
        let unused = match kind {
            Write::Create => quote!(let _ = (db, __access, __form);),
            Write::Update => quote!(let _ = (db, __access, __id, __form);),
        };
        return quote!(#signature { #unused #body });
    }

    let stored = match kind {
        Write::Create => None,
        Write::Update => Some(quote!(__stored)),
    };
    let parse_stmts: Vec<TokenStream> = form_cols.iter().map(|c| parse_stmt(c)).collect();
    let checks: Vec<TokenStream> = form_cols
        .iter()
        .map(|c| validate_stmt(c, stored.as_ref()))
        .collect();
    let unique_checks: Vec<TokenStream> = form_cols
        .iter()
        .filter(|c| c.def.unique && !c.orm_text && !c.def.json)
        .map(|c| unique_check(ident, c, kind))
        .collect();
    let slug_fills: Vec<TokenStream> = form_cols
        .iter()
        .filter_map(|c| {
            let source = c.ui.slug_from.as_ref()?;
            let name = &c.def.name;
            let fid = field_ident(name);
            let stored = match kind {
                Write::Create => quote!(::core::option::Option::None),
                Write::Update if c.def.nullable => quote!(__stored.#fid.as_deref()),
                Write::Update => quote!(::core::option::Option::Some(__stored.#fid.as_str())),
            };
            Some(quote! {
                #models::form::fill_slug(&mut __submitted, #name, #source, #stored);
            })
        })
        .collect();

    // Create: an omitted value of a NOT NULL column with a literal default
    // takes the default (the form marks those columns optional). Booleans
    // are excluded — an unchecked box and an absent field look the same.
    let default_fills: Vec<TokenStream> = if kind == Write::Create {
        form_cols
            .iter()
            .filter(|c| !c.def.nullable && c.def.ty != SqlType::Boolean)
            .filter_map(|c| {
                let value = match c.def.default.as_ref()? {
                    DefaultValue::Int(i) => i.to_string(),
                    DefaultValue::Float(f) => f.to_string(),
                    DefaultValue::Text(t) => t.clone(),
                    DefaultValue::Bool(_) | DefaultValue::Now => return None,
                };
                let name = &c.def.name;
                Some(quote!(#models::form::fill_default(&mut __submitted, #name, #value);))
            })
            .collect()
    } else {
        Vec::new()
    };

    let mut assigns: Vec<TokenStream> = form_cols
        .iter()
        .map(|c| {
            let fid = field_ident(&c.def.name);
            let var = format_ident!("__v_{}", c.def.name);
            quote!(#fid = #var.unwrap())
        })
        .collect();
    if kind == Write::Create {
        if let Some(owner) = cols.iter().find(|c| c.is_owner) {
            let fid = field_ident(&owner.def.name);
            assigns.push(if owner.def.nullable {
                quote!(#fid = ::core::option::Option::Some(__user.id()))
            } else {
                quote!(#fid = __user.id())
            });
        }
        if let Some(parent) = cols.iter().find(|c| c.is_parent) {
            let fid = field_ident(&parent.def.name);
            assigns.push(quote!(#fid = __parent_id));
        }
    }
    let parent_id = if kind == Write::Create && cols.iter().any(|c| c.is_parent) {
        quote! {
            let ::core::option::Option::Some(__parent_id) = __access.parent_id else {
                return ::core::result::Result::Err(#models::AppError::BadRequest(
                    "a nested row is created under its parent".to_string(),
                ));
            };
        }
    } else {
        quote!()
    };

    // Access checks before anything is parsed: create asks can_create; an
    // update must find the row inside the user's scope and pass can_edit.
    let (guard, existing) = match kind {
        Write::Create => (
            quote! {
                #parent_id
                if !#hooks::can_create(db, __user, __access.parent_id).await? {
                    return ::core::result::Result::Err(#models::AppError::NoAuth);
                }
            },
            quote!(::core::option::Option::None),
        ),
        Write::Update => (
            quote! {
                let ::core::option::Option::Some(__stored) =
                    __fse_fetch(db, __access, __id).await?
                else {
                    return ::core::result::Result::Err(__fse_not_found());
                };
                if !#hooks::can_edit(&__stored, db, __user).await? {
                    return ::core::result::Result::Err(#models::AppError::NoAuth);
                }
            },
            quote!(::core::option::Option::Some(&__stored)),
        ),
    };

    let tail = match kind {
        Write::Create => quote! {
            let __row = ::fse_orm::insert!(#ident, db, #(#assigns),*).await?;
            #hooks::after_save(&__row, db, __user, true).await?;
            ::core::result::Result::Ok(::core::result::Result::Ok(__row.id))
        },
        Write::Update => quote! {
            ::fse_orm::update!(#ident, db, id == __id; #(#assigns),*).await?;
            let __row = #ident::fetch(db, __id).await?.ok_or_else(__fse_not_found)?;
            #hooks::after_save(&__row, db, __user, false).await?;
            ::core::result::Result::Ok(::core::result::Result::Ok(()))
        },
    };

    quote! {
        #signature {
            ::std::boxed::Box::pin(async move {
                let ::core::option::Option::Some(__user) = __access.user else {
                    return ::core::result::Result::Err(#models::AppError::NoAuth);
                };
                #guard
                let mut __submitted: #models::FormData = __form.clone();
                let mut __errors: #models::FormErrors = #hooks::before_save(
                    &mut __submitted,
                    #models::SaveCx {
                        db,
                        user: __user,
                        existing: #existing,
                        parent_id: __access.parent_id,
                    },
                )
                .await?;
                #(#slug_fills)*
                #(#default_fills)*
                let __form = &__submitted;
                #(#parse_stmts)*
                #(#checks)*
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
/// whose `None` means "invalid, error recorded"; nullable columns hold an
/// inner `Option` (`Some(None)` = left empty).
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
            SqlType::Real if c.def.rust_type == "f64" => {
                if nullable {
                    quote!(#form::opt_decimal(__form, #name, &mut __errors))
                } else {
                    quote!(#form::req_decimal(__form, #name, &mut __errors))
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

/// Declarative validation of one parsed field — `#[ui(required)]` on an
/// optional column, `email`, `url`, `min`/`max` — plus, for a foreign key,
/// that the referenced row is one the user may read (an unchanged value on
/// update is kept as is).
fn validate_stmt(c: &ColInfo, stored: Option<&TokenStream>) -> TokenStream {
    let name = &c.def.name;
    let var = format_ident!("__v_{}", c.def.name);
    let form = quote!(::full_stack_engine::models::form);
    let present = if c.def.nullable {
        quote!(#var.as_ref().and_then(|__x| __x.as_ref()))
    } else {
        quote!(#var.as_ref())
    };
    let mut out = Vec::new();

    if c.ui.required && c.def.nullable {
        out.push(quote! {
            if ::core::matches!(#var, ::core::option::Option::Some(::core::option::Option::None)) {
                #form::push_error(&mut __errors, #name, "required");
            }
        });
    }
    let string = c.def.rust_type == "String" && c.def.ty == SqlType::Text && !c.def.is_enum;
    if c.ui.email {
        out.push(quote! {
            if let ::core::option::Option::Some(__x) = #present {
                if !#form::valid_email(__x) {
                    #form::push_error(&mut __errors, #name, "invalid_email");
                }
            }
        });
    }
    if c.ui.url {
        out.push(quote! {
            if let ::core::option::Option::Some(__x) = #present {
                if !#form::valid_url(__x) {
                    #form::push_error(&mut __errors, #name, "invalid_url");
                }
            }
        });
    }
    if c.ui.min.is_some() || c.ui.max.is_some() {
        let min = opt_f64(c.ui.min);
        let max = opt_f64(c.ui.max);
        out.push(if string {
            quote! {
                if let ::core::option::Option::Some(__x) = #present {
                    #form::check_length(&mut __errors, #name, __x, #min, #max);
                }
            }
        } else {
            quote! {
                if let ::core::option::Option::Some(__x) = #present {
                    #form::check_range(&mut __errors, #name, #form::AsF64::as_f64(__x), #min, #max);
                }
            }
        });
    }
    if let Some(target) = c.references() {
        let fid = field_ident(name);
        let unchanged = match stored {
            Some(s) if c.def.nullable => {
                quote!(#s.#fid == ::core::option::Option::Some(*__x))
            }
            Some(s) => quote!(#s.#fid == *__x),
            None => quote!(false),
        };
        out.push(quote! {
            if let ::core::option::Option::Some(__x) = #present {
                if !(#unchanged)
                    && !::full_stack_engine::models::ref_visible(db, __user, #target, *__x).await?
                {
                    #form::push_error(&mut __errors, #name, "invalid_option");
                }
            }
        });
    }
    quote!(#(#out)*)
}

fn opt_f64(v: Option<f64>) -> TokenStream {
    match v {
        Some(x) => quote!(::core::option::Option::Some(#x)),
        None => quote!(::core::option::Option::None),
    }
}

/// Pre-check a unique column so a duplicate becomes a `not_unique` field
/// error on the re-rendered form instead of a raw constraint violation.
/// Updates exclude the row being edited.
fn unique_check(ident: &syn::Ident, c: &ColInfo, kind: Write) -> TokenStream {
    let name = &c.def.name;
    let var = format_ident!("__v_{}", c.def.name);
    let cid = col_const(&c.def.name);
    let push = quote! {
        ::full_stack_engine::models::form::push_error(&mut __errors, #name, "not_unique");
    };
    let guard = match kind {
        Write::Create => push,
        Write::Update => quote!(if __existing.id != __id { #push }),
    };
    let pattern = if c.def.nullable {
        quote!(::core::option::Option::Some(::core::option::Option::Some(
            __u
        )))
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
    let models = quote!(::full_stack_engine::models);
    let hooks = quote!(<#ident as #models::ModelHooks>);
    quote! {
        fn delete<'a>(
            &'a self,
            db: &'a #models::Db,
            __access: #models::Access<'a>,
            id: i64,
        ) -> #models::BoxFuture<'a, #models::AppResult<()>> {
            ::std::boxed::Box::pin(async move {
                let ::core::option::Option::Some(__user) = __access.user else {
                    return ::core::result::Result::Err(#models::AppError::NoAuth);
                };
                let ::core::option::Option::Some(__stored) = __fse_fetch(db, __access, id).await?
                else {
                    return ::core::result::Result::Err(__fse_not_found());
                };
                if !#hooks::can_delete(&__stored, db, __user).await? {
                    return ::core::result::Result::Err(#models::AppError::NoAuth);
                }
                #hooks::before_delete(&__stored, db, __user).await?;
                #ident::delete(db, id).await?;
                #hooks::after_delete(__stored, db, __user).await?;
                ::core::result::Result::Ok(())
            })
        }
    }
}

/// `act`: dispatch a row action to `async fn name(&self, ActionCx)` on the
/// model, after the scope and `can_act`.
fn emit_act(ident: &syn::Ident, opts: &ModelOpts) -> TokenStream {
    let models = quote!(::full_stack_engine::models);
    let hooks = quote!(<#ident as #models::ModelHooks>);
    let arms = opts.actions.iter().map(|a| {
        let name = a.to_string();
        quote! {
            #name => {
                if !#hooks::can_act(&__row, #name, db, __user).await? {
                    return ::core::result::Result::Err(#models::AppError::NoAuth);
                }
                #ident::#a(&__row, #models::ActionCx { db, user: __user, form: __form }).await
            }
        }
    });
    quote! {
        fn act<'a>(
            &'a self,
            db: &'a #models::Db,
            __access: #models::Access<'a>,
            __id: i64,
            __action: &'a str,
            __form: &'a #models::FormData,
        ) -> #models::BoxFuture<'a, #models::AppResult<()>> {
            ::std::boxed::Box::pin(async move {
                let ::core::option::Option::Some(__user) = __access.user else {
                    return ::core::result::Result::Err(#models::AppError::NoAuth);
                };
                let ::core::option::Option::Some(__row) = __fse_fetch(db, __access, __id).await?
                else {
                    return ::core::result::Result::Err(__fse_not_found());
                };
                #[allow(unreachable_code, clippy::match_single_binding)]
                match __action {
                    #(#arms)*
                    _ => {
                        let _ = (&__row, __user, __form);
                        ::core::result::Result::Err(__fse_not_found())
                    }
                }
            })
        }
    }
}
