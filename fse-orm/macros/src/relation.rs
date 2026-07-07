//! `include:` — Prisma-style eager relation loading for `find!`/`find_one!`.
//!
//! A relation field (`#[orm(relation = fk_column)] field: Option<Target>`) is
//! not a database column; it is populated by a real SQL JOIN generated at
//! macro-expansion time and passed through a literal `sqlx::query!` call, so
//! sqlx still checks the whole query — including the joined columns — against
//! the real schema.
//!
//! The tricky part is naming `Target` in the generated code: `find!`/
//! `find_one!` expand at an arbitrary call site (possibly in a different
//! crate, e.g. an integration test binary), which only has `#table_ident`
//! (typed literally by the caller) in scope — never the relation's target
//! type, since the caller only ever wrote the lowercase field name
//! (`include: [product]`, not `Product`). So the code that builds a `Target`
//! value lives instead in `derive(Table)`'s expansion for the relation's
//! *owning* struct (as a hidden associated function), which is spliced into
//! that struct's own source file — the same place its field already had to
//! import `Target` for `Option<Target>` to type-check at all. `find!` then
//! only ever calls `#table_ident::__fse_relation_<field>(..)`, never naming
//! `Target` itself.

use fse_schema::{ColumnDef, RelationDef, TableDef};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};

use crate::{codegen, lookup};

pub struct Included {
    pub relation: RelationDef,
    pub target: TableDef,
}

/// Resolve `include: [a, b]` identifiers against `table`'s declared
/// relations, loading each target table's own definition (for its columns).
pub fn resolve(table: &TableDef, idents: &[syn::Ident]) -> syn::Result<Vec<Included>> {
    let mut out = Vec::new();
    for ident in idents {
        let name = ident.to_string();
        let Some(relation) = table.relation(&name).cloned() else {
            return Err(syn::Error::new(
                ident.span(),
                format!(
                    "no relation `{name}` on table `{}` (declare it with a field \
                     `#[orm(relation = ...)] {name}: Option<Target>`)",
                    table.name
                ),
            ));
        };
        let target = lookup::load_table(&relation.target_struct, ident.span())?;
        out.push(Included { relation, target });
    }
    Ok(out)
}

/// `JOIN events AS run ON registrations.run_id = run.id` — or `LEFT JOIN`
/// when the foreign key column is nullable (the relation may be absent).
/// The join alias is always the relation's field name, so two relations to
/// the same table (`Donation::donor`, `Donation::runner`, both `-> users`)
/// never collide.
pub fn join_clause(table_name: &str, inc: &Included) -> String {
    let kind = if inc.relation.nullable { "LEFT JOIN" } else { "JOIN" };
    format!(
        "{kind} {} AS {} ON {table_name}.{} = {}.id",
        inc.target.name, inc.relation.field, inc.relation.local_column, inc.relation.field
    )
}

/// The joined table's columns, added to the SELECT list.
pub fn select_items(inc: &Included) -> Vec<String> {
    inc.target
        .columns
        .iter()
        .map(|c| codegen::select_item_relation(c, &inc.relation.field, inc.relation.nullable))
        .collect()
}

fn helper_fn_ident(field: &str) -> syn::Ident {
    format_ident!("__fse_relation_{field}")
}

/// The hidden associated function `derive(Table)` emits, once per relation
/// field, on the relation's *owning* struct (e.g. `Review::
/// __fse_relation_product`). Defined in the owner's own module, so `Target`
/// (`Product`) resolves through whatever import the owner's own field
/// declaration already required. Takes each of the target's columns as a
/// plain, portable-typed (see `codegen::portable_rust_type`) raw value —
/// never the target's own domain types (an enum, a json payload's inner
/// type) — those are inferred from the `Target { .. }` struct-literal
/// context inside the function body, the same way `codegen::build_field`
/// already infers them for a table's own `fetch`/`fetch_all`.
pub fn helper_fn_def(inc: &Included, span: Span) -> syn::Result<TokenStream> {
    let helper_ident = helper_fn_ident(&inc.relation.field);
    let target_path = lookup::table_path(&inc.target.struct_name, span)?;
    let force_nullable = inc.relation.nullable;

    let param_idents: Vec<syn::Ident> =
        inc.target.columns.iter().map(|c| format_ident!("{}", c.name)).collect();
    let param_types: Vec<TokenStream> = inc
        .target
        .columns
        .iter()
        .map(|c| codegen::portable_rust_type(c, force_nullable))
        .collect();
    let field_inits: Vec<TokenStream> = inc
        .target
        .columns
        .iter()
        .zip(&param_idents)
        .map(|(c, id)| build_field_from_param(c, id, force_nullable))
        .collect();
    // The target's own relation fields are never loaded transitively by a
    // single-level `include:` — they start `None`, same as any other fetch.
    let relation_defaults: Vec<TokenStream> = inc
        .target
        .relations
        .iter()
        .map(|r| {
            let id = format_ident!("{}", r.field);
            quote! { #id: None }
        })
        .collect();

    Ok(quote! {
        #[doc(hidden)]
        #[allow(clippy::too_many_arguments)]
        pub fn #helper_ident(
            has_row: bool,
            #(#param_idents: #param_types),*
        ) -> ::sqlx::Result<::std::option::Option<#target_path>> {
            if !has_row {
                return ::std::result::Result::Ok(::std::option::Option::None);
            }
            ::std::result::Result::Ok(::std::option::Option::Some(#target_path {
                #(#field_inits,)*
                #(#relation_defaults),*
            }))
        }
    })
}

/// One field initializer inside the helper's struct literal: converts the
/// raw parameter into the target field's real type. `force_unwrap` covers a
/// column that is NOT NULL in its own right but still arrives as
/// `Option<_>` because the whole joined row is NULL under a non-matching
/// LEFT JOIN; unwrapping it is safe because `helper_fn_def` only reaches
/// this code once `has_row` has already established the relation matched.
fn build_field_from_param(c: &ColumnDef, param: &syn::Ident, force_nullable: bool) -> TokenStream {
    let id = format_ident!("{}", c.name);
    let name = &c.name;
    let force_unwrap = force_nullable && !c.nullable;
    let unwrap = quote! { .expect("left join: presence implied by relation pk") };

    if c.json {
        if c.nullable {
            quote! { #id: ::fse_orm::opt_from_json_str(#name, #param.as_deref())? }
        } else if force_unwrap {
            quote! { #id: ::fse_orm::from_json_str(#name, &#param #unwrap)? }
        } else {
            quote! { #id: ::fse_orm::from_json_str(#name, &#param)? }
        }
    } else if c.is_enum {
        if c.nullable {
            quote! { #id: ::fse_orm::opt_parse_db_value(#name, #param.as_deref())? }
        } else if force_unwrap {
            quote! { #id: ::fse_orm::parse_db_value(#name, &#param #unwrap)? }
        } else {
            quote! { #id: ::fse_orm::parse_db_value(#name, &#param)? }
        }
    } else if force_unwrap {
        quote! { #id: #param #unwrap }
    } else {
        quote! { #id: #param }
    }
}

/// `Review::__fse_relation_product(has_row, r.product__id, r.product__slug,
/// ..)?` — the call `find!`/`find_one!` emits for one included relation.
/// `owner` is `#table_ident` from the macro invocation, always already in
/// scope (the caller wrote it literally); nothing else needs to be nameable
/// at this call site.
pub fn call_expr(owner: &syn::Ident, inc: &Included) -> TokenStream {
    let helper_ident = helper_fn_ident(&inc.relation.field);
    let has_row: TokenStream = if inc.relation.nullable {
        let pk_field = format_ident!("{}__id", inc.relation.field);
        quote! { r.#pk_field.is_some() }
    } else {
        quote! { true }
    };
    let row_args: Vec<TokenStream> = inc
        .target
        .columns
        .iter()
        .map(|c| {
            let row_field = format_ident!("{}__{}", inc.relation.field, c.name);
            quote! { r.#row_field }
        })
        .collect();
    quote! { #owner::#helper_ident(#has_row, #(#row_args),*)? }
}
