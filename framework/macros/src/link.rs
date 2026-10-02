//! Expansion of `#[model(link = owner_column)]` — a many-to-many join table.
//!
//! The struct links two models through two foreign keys: `owner_column`
//! names the model whose rows own the links (the "event" in event ↔
//! managers), the other foreign key the linked model. The framework mounts
//! the link pages under the owner row (`/event-manager/{id}/managers`);
//! adding and removing a link insert and delete the join row, so the join
//! struct's own `ModelHooks` (`can_create`, `after_save`, `can_delete`,
//! `after_delete`) apply.

use fse_schema::TableDef;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::model::{ModelOpts, default_hooks, opt_str, struct_with_derives, table_json};

pub fn expand(
    item: &syn::ItemStruct,
    table: &TableDef,
    opts: &ModelOpts,
) -> syn::Result<TokenStream> {
    let ident = &item.ident;
    let owner_col = opts.link.as_deref().expect("link set");
    let err = |msg: String| syn::Error::new(ident.span(), msg);

    let fks: Vec<_> = table
        .columns
        .iter()
        .filter(|c| c.references.is_some())
        .collect();
    let others: Vec<_> = fks.iter().filter(|c| c.name != owner_col).collect();
    let [other] = others.as_slice() else {
        return Err(err(format!(
            "a link table needs exactly two foreign keys — `{owner_col}` and the linked \
             model's — found {}",
            fks.len()
        )));
    };
    if other.rust_type != "i64" || other.nullable {
        return Err(err(format!(
            "link column `{}` must be a NOT NULL `i64` foreign key",
            other.name
        )));
    }
    let owner = table.column(owner_col).expect("validated in model_opts");
    if owner.nullable {
        return Err(err(format!("link column `{owner_col}` must be NOT NULL")));
    }
    // Every other NOT NULL column needs a default, or adding a link (which
    // only knows the two ids) could never insert.
    if let Some(c) = table.columns.iter().find(|c| {
        c.name != owner_col
            && c.name != other.name
            && !c.primary_key
            && !c.nullable
            && c.default.is_none()
    }) {
        return Err(err(format!(
            "link table column `{}` is NOT NULL without a default — adding a link only sets \
             the two ids",
            c.name
        )));
    }

    let owner_struct = owner.references.as_ref().expect("fk").table.clone();
    let other_struct = other.references.as_ref().expect("fk").table.clone();
    let owner_cid = format_ident!("{}", owner_col.to_uppercase());
    let other_cid = format_ident!("{}", other.name.to_uppercase());
    let owner_fid = format_ident!("{}", owner_col);
    let other_fid = format_ident!("{}", other.name);
    let models = quote!(::full_stack_engine::models);
    let hooks = quote!(<#ident as #models::ModelHooks>);

    // Find the join row: by its composite key when that is exactly the two
    // ids, else through the dynamic builder; delete it the same way.
    let pk: Vec<&str> = table
        .primary_key()
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let (find, delete) = if table.auto_id() {
        (
            quote! {
                #ident::find()
                    .filter(#ident::#owner_cid.eq(__owner))
                    .filter(#ident::#other_cid.eq(__other))
                    .fetch_optional(db)
                    .await?
            },
            quote!(#ident::delete(db, __row.id).await?;),
        )
    } else if pk.len() == 2 && pk.contains(&owner_col) && pk.contains(&other.name.as_str()) {
        let args: Vec<TokenStream> = pk
            .iter()
            .map(|c| {
                if *c == owner_col {
                    quote!(__owner)
                } else {
                    quote!(__other)
                }
            })
            .collect();
        (
            quote!(#ident::fetch(db, #(#args),*).await?),
            quote!(#ident::delete(db, #(#args),*).await?;),
        )
    } else {
        return Err(err(format!(
            "a link table's primary key must be `id` or exactly ({owner_col}, {})",
            other.name
        )));
    };

    let emitted_struct = struct_with_derives(item);
    let default_hooks = default_hooks(ident, opts.hooks);
    let table_json = table_json(item, table)?;
    let path = opt_str(opts.path.as_deref());
    let owner_name = owner_col.to_string();
    let other_name = other.name.clone();

    Ok(quote! {
        #emitted_struct

        const _: () = {
            static __FSE_LINK_UI: #models::UiLink = #models::UiLink {
                path: #path,
                owner_column: #owner_name,
                owner: #owner_struct,
                other_column: #other_name,
                other: #other_struct,
            };

            #default_hooks

            struct __FseLinkResource;
            static __FSE_LINK_RESOURCE: __FseLinkResource = __FseLinkResource;

            impl #models::LinkResource for __FseLinkResource {
                fn linked<'a>(
                    &'a self,
                    db: &'a #models::Db,
                    __owner: i64,
                ) -> #models::BoxFuture<'a, #models::AppResult<::std::vec::Vec<i64>>> {
                    ::std::boxed::Box::pin(async move {
                        ::core::result::Result::Ok(
                            #ident::find()
                                .filter(#ident::#owner_cid.eq(__owner))
                                .fetch_all(db)
                                .await?
                                .into_iter()
                                .map(|__r| __r.#other_fid)
                                .collect(),
                        )
                    })
                }

                fn add<'a>(
                    &'a self,
                    db: &'a #models::Db,
                    __user: &'a #models::CurrentUser,
                    __owner: i64,
                    __other: i64,
                ) -> #models::BoxFuture<'a, #models::AppResult<bool>> {
                    ::std::boxed::Box::pin(async move {
                        if !#hooks::can_create(db, __user, ::core::option::Option::Some(__owner))
                            .await?
                        {
                            return ::core::result::Result::Err(#models::AppError::NoAuth);
                        }
                        if (#find).is_some() {
                            return ::core::result::Result::Ok(false);
                        }
                        let __row = ::fse_orm::insert!(
                            #ident,
                            db,
                            #owner_fid = __owner,
                            #other_fid = __other
                        )
                        .await?;
                        #hooks::after_save(&__row, db, __user, true).await?;
                        ::core::result::Result::Ok(true)
                    })
                }

                fn remove<'a>(
                    &'a self,
                    db: &'a #models::Db,
                    __user: &'a #models::CurrentUser,
                    __owner: i64,
                    __other: i64,
                ) -> #models::BoxFuture<'a, #models::AppResult<bool>> {
                    ::std::boxed::Box::pin(async move {
                        let ::core::option::Option::Some(__row) = (#find) else {
                            return ::core::result::Result::Ok(false);
                        };
                        if !#hooks::can_delete(&__row, db, __user).await? {
                            return ::core::result::Result::Err(#models::AppError::NoAuth);
                        }
                        #hooks::before_delete(&__row, db, __user).await?;
                        #delete
                        #hooks::after_delete(__row, db, __user).await?;
                        ::core::result::Result::Ok(true)
                    })
                }
            }

            ::full_stack_engine::inventory::submit! {
                #models::LinkRegistration {
                    table_json: #table_json,
                    ui: &__FSE_LINK_UI,
                    resource: &__FSE_LINK_RESOURCE,
                }
            }
        };
    })
}
