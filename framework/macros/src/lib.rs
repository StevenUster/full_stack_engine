//! Proc macros for the full_stack_engine framework.
//!
//! `#[derive(Model)]` turns a `#[derive(Table)]` struct into an app
//! definition: the struct is re-parsed with the same fse-schema code the ORM
//! derive uses (so the database meaning can never drift), the framework-owned
//! `#[model(...)]` and `#[ui(...)]` attributes are parsed on top, and the
//! combined metadata is registered in the framework's runtime model registry
//! (`full_stack_engine::models`). At boot the framework mounts generic CRUD
//! routes for every registered model; the generic templates render lists and
//! forms from the same metadata.

use proc_macro::TokenStream;

mod model;

/// Marks a `#[derive(Table)]` struct as an app model: its metadata is
/// registered in `full_stack_engine::models` and the framework generates
/// admin CRUD endpoints and pages for it at boot.
///
/// Struct-level `#[model(...)]` keys:
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
/// Everything expressible here is validated at compile time; mistakes such as
/// naming a column that does not exist are build errors, not runtime
/// surprises.
#[proc_macro_derive(Model, attributes(model, ui, orm))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let item = syn::parse_macro_input!(input as syn::ItemStruct);
    model::expand(&item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
