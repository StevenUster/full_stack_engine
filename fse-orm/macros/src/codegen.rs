//! Column-level codegen shared by `derive(Table)` and the query macros:
//! the SELECT list with typed overrides and the record → struct conversion.
//!
//! Everything emitted here must work at *any* call site, without imports:
//! type overrides only name primitives/prelude types or absolute
//! `::sqlx::types::...` paths, and enum columns come back as TEXT and are
//! parsed via their `DbEnum`-generated `FromStr` (whose target type is
//! inferred from the struct field).

use fse_schema::ColumnDef;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// One item of a SELECT/RETURNING list, with `!` for NOT NULL columns and a
/// type override where inference would not match the struct field.
pub fn select_item(c: &ColumnDef) -> String {
    let name = &c.name;
    let bang = if c.nullable { "" } else { "!" };
    match override_type(c) {
        Some(ty) => format!("{name} as \"{name}{bang}: {ty}\""),
        None if bang.is_empty() => name.clone(),
        None => format!("{name} as \"{name}{bang}\""),
    }
}

/// The type named in the override — always resolvable without imports.
/// json and enum columns get none (TEXT comes back as `String`).
fn override_type(c: &ColumnDef) -> Option<String> {
    if c.json || c.is_enum {
        return None;
    }
    Some(match c.rust_type.as_str() {
        "NaiveDateTime" => "::sqlx::types::chrono::NaiveDateTime".into(),
        "NaiveDate" => "::sqlx::types::chrono::NaiveDate".into(),
        "NaiveTime" => "::sqlx::types::chrono::NaiveTime".into(),
        t if t.starts_with("DateTime") => {
            "::sqlx::types::chrono::DateTime<::sqlx::types::chrono::Utc>".into()
        }
        "Uuid" => "::sqlx::types::Uuid".into(),
        // Primitives, String, Vec<u8>: in scope everywhere.
        other => other.into(),
    })
}

pub fn select_list(columns: &[ColumnDef]) -> String {
    columns.iter().map(select_item).collect::<Vec<_>>().join(", ")
}

/// `field: r.field` — with serde conversion for json columns and `FromStr`
/// for enum columns. Expects the query record to be bound to `r`.
pub fn build_field(c: &ColumnDef) -> TokenStream {
    let id = format_ident!("{}", c.name);
    let name = &c.name;
    if c.json {
        if c.nullable {
            quote! { #id: ::fse_orm::opt_from_json_str(#name, r.#id.as_deref())? }
        } else {
            quote! { #id: ::fse_orm::from_json_str(#name, &r.#id)? }
        }
    } else if c.is_enum {
        if c.nullable {
            quote! { #id: ::fse_orm::opt_parse_db_value(#name, r.#id.as_deref())? }
        } else {
            quote! { #id: ::fse_orm::parse_db_value(#name, &r.#id)? }
        }
    } else {
        quote! { #id: r.#id }
    }
}

pub fn build_fields(columns: &[ColumnDef]) -> Vec<TokenStream> {
    columns.iter().map(build_field).collect()
}
