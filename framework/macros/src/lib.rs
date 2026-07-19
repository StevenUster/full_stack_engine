//! Proc macros for the full_stack_engine framework.
//!
//! `#[model(...)]` turns a plain struct into a complete app definition. It
//! expands to the struct itself with `#[derive(Table, Debug, Clone)]`
//! attached (any derive the struct already has is not duplicated), so the
//! fse ORM generates the schema-checked data layer, plus a registration in
//! the framework's runtime model registry (`full_stack_engine::models`) from
//! which the framework generates admin CRUD endpoints and pages at boot.
//! One annotation defines database, endpoints and UI.

use proc_macro::TokenStream;

mod model;
mod resource;

/// Marks a struct as an app model: the ORM `Table` derive is applied for the
/// data layer, and the struct's metadata is registered in
/// `full_stack_engine::models` so the framework generates admin CRUD
/// endpoints and pages for it at boot.
///
/// Arguments (all optional):
/// - `permission = "products"` — base permission name (default: table name);
///   generated routes check `<base>.read` / `<base>.write`.
/// - `path = "product-manager"` — base URL path segment (default: table name).
/// - `public_read` / `public_read = slug` — additionally expose public
///   read-only pages, looked up by the given unique column (bare = primary
///   key).
/// - `api` — additionally expose the JSON API endpoints.
/// - `disabled` — register metadata only, generate no routes.
/// - `no_create` / `no_edit` / `no_delete` — switch off individual generated
///   endpoints.
/// - `title_field = name` — column used as the row title on detail pages
///   (default: the first plain text column, else the primary key).
///
/// Field-level `#[ui(...)]` keys (all bare flags):
/// - `list` — show this column in the generated list table. If no field is
///   marked, every scalar non-secret column except the primary key is shown.
/// - `search` — the list search box matches this (plain text) column.
/// - `filter` — offer a filter dropdown (`DbEnum` and `bool` columns).
/// - `textarea` — render a multi-line editor for this text column.
/// - `hidden` — never show the column in generated UI.
/// - `readonly` — show the column but never edit it in generated forms.
///
/// `#[orm(...)]` attributes work exactly as with a hand-written
/// `#[derive(Table)]` struct — `#[model]` structs are what `fse migrate`
/// generates migrations from.
///
/// Everything expressible here is validated at compile time; mistakes such as
/// naming a column that does not exist are build errors, not runtime
/// surprises.
#[proc_macro_attribute]
pub fn model(args: TokenStream, input: TokenStream) -> TokenStream {
    let item = syn::parse_macro_input!(input as syn::ItemStruct);
    model::expand(args.into(), &item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
