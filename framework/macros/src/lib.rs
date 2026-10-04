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

mod link;
mod model;
mod resource;
mod themes;

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
/// - `owner = user_id` — rows belong to the user in this `i64` column:
///   non-admins (`Role::is_admin`) only ever see and change their own rows,
///   and on create the column is filled from the signed-in user (it is never
///   a form field, so it can't be spoofed).
/// - `hooks` — the app implements `full_stack_engine::models::ModelHooks`
///   for this struct: row-level access (`scope`, `can_edit`, ...), lifecycle
///   hooks (`before_save`, `after_delete`, ...) and computed display fields.
///   Without it every hook keeps its default.
///
/// Field-level `#[ui(...)]` keys:
/// - `list` — show this column in the generated list table. If no field is
///   marked, every scalar non-secret column except the primary key is shown.
/// - `search` — the list search box matches this (plain text) column.
/// - `filter` — offer a list filter: a dropdown for `DbEnum` and `bool`
///   columns, a substring match for text, a `{col}_from`/`{col}_to` range
///   for numbers, dates and timestamps. `filter = exact|contains|range`
///   picks the kind explicitly (e.g. a range over ISO dates kept as text).
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

/// Every theme in the app's `themes/` folder, embedded in the binary, as a
/// `full_stack_engine::themes::ThemeSet` — with the active theme named by
/// `fse.toml`:
///
/// ```toml
/// [themes]
/// active = "my-app"                    # a theme.json name; default: the one nothing extends
/// # dir = "themes"                     # the folder, relative to the crate root
/// # dev_server = "http://localhost:4321"
/// ```
///
/// Each subfolder is one theme: an npm-built theme (Astro with `fse-ssr`,
/// …) contributes its built `dist/` and gets `dev_server` for `ENV=dev`; a
/// folder without a `package.json` is a hand-written theme and is embedded
/// as it is. A theme may extend another theme of the folder or an installed
/// theme crate; add the crates with `.with(...)`:
///
/// ```ignore
/// pub fn themes() -> ThemeSet {
///     full_stack_engine::themes!().with(Theme::embedded(&fse_theme_default::DIST))
/// }
/// ```
///
/// A folder that is no theme is a compile error. An npm theme that was never
/// built still compiles (so `cargo run --bin dev`, which builds it, can
/// start), and boot fails with a "run the build" message if the active
/// theme needs it. Edits to `fse.toml` and to built files recompile the
/// crate; a brand-new theme folder is picked up by the next build that
/// recompiles it.
#[proc_macro]
pub fn themes(input: TokenStream) -> TokenStream {
    themes::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
