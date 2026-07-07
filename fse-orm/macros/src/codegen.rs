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
    select_item_impl(c, None)
}

/// Same as [`select_item`], but the FROM-side reference is qualified
/// (`{qualifier}.{name}`) while the result column keeps the bare name — used
/// for a query's own table once a join makes an unqualified column ambiguous.
/// Produces byte-identical SQL to [`select_item`] when `qualifier` is `None`,
/// so queries that don't join anything don't churn the offline query cache.
fn select_item_impl(c: &ColumnDef, qualifier: Option<&str>) -> String {
    let name = &c.name;
    let source = match qualifier {
        Some(q) => format!("{q}.{name}"),
        None => name.clone(),
    };
    let bang = if c.nullable { "" } else { "!" };
    match override_type(c) {
        Some(ty) => format!("{source} as \"{name}{bang}: {ty}\""),
        None if bang.is_empty() => source,
        None => format!("{source} as \"{name}{bang}\""),
    }
}

/// A joined relation's column: qualified by the join alias (the relation's
/// field name) on the FROM side, and re-aliased `{alias}__{col}` in the
/// result so it can never collide with the primary table's own (bare) column
/// names or another relation's. A LEFT JOIN (`force_nullable`, from a
/// nullable foreign key) forces every column nullable in the override — a
/// non-matching row means the whole joined row, including its own NOT NULL
/// columns, comes back NULL. That forcing must be explicit (sqlx's `?`
/// marker), not left to sqlx's own nullability inference: its SQLite
/// analysis does not account for a LEFT JOIN turning an otherwise-NOT-NULL
/// column nullable, and would otherwise hand back a plain `T` here — which
/// `relation::helper_fn_def`'s generated parameter types (always `Option<T>`
/// under `force_nullable`) would then mismatch.
pub fn select_item_relation(c: &ColumnDef, alias: &str, force_nullable: bool) -> String {
    let out_name = format!("{alias}__{}", c.name);
    let marker = if force_nullable {
        "?"
    } else if c.nullable {
        ""
    } else {
        "!"
    };
    match override_type(c) {
        Some(ty) => format!("{alias}.{} as \"{out_name}{marker}: {ty}\"", c.name),
        None if marker.is_empty() => format!("{alias}.{} as \"{out_name}\"", c.name),
        None => format!("{alias}.{} as \"{out_name}{marker}\"", c.name),
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

/// [`select_list`], with every column's FROM-side reference qualified by
/// `qualifier` — needed once a join is present and a bare column name would
/// be ambiguous between the primary table and a joined one.
pub fn select_list_qualified(columns: &[ColumnDef], qualifier: &str) -> String {
    columns
        .iter()
        .map(|c| select_item_impl(c, Some(qualifier)))
        .collect::<Vec<_>>()
        .join(", ")
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

/// The portable Rust type of a column's *raw* value — same absolute-path
/// rule as [`override_type`], but as a type (for a function signature)
/// rather than a SQL override string. json/enum columns come back as
/// `String` (their raw TEXT form); the caller applies
/// `from_json_str`/`parse_db_value` to reach the real domain type, which is
/// inferred from context rather than spelled — see
/// `relation::build_field_from_param`, the only caller that needs this for a
/// type it cannot otherwise name.
pub fn portable_rust_type(c: &ColumnDef, force_nullable: bool) -> TokenStream {
    let inner: TokenStream = if c.json || c.is_enum {
        quote! { ::std::string::String }
    } else {
        match c.rust_type.as_str() {
            "NaiveDateTime" => quote! { ::sqlx::types::chrono::NaiveDateTime },
            "NaiveDate" => quote! { ::sqlx::types::chrono::NaiveDate },
            "NaiveTime" => quote! { ::sqlx::types::chrono::NaiveTime },
            t if t.starts_with("DateTime") => {
                quote! { ::sqlx::types::chrono::DateTime<::sqlx::types::chrono::Utc> }
            }
            "Uuid" => quote! { ::sqlx::types::Uuid },
            "String" => quote! { ::std::string::String },
            "Vec<u8>" => quote! { ::std::vec::Vec<u8> },
            other => {
                let ident = format_ident!("{other}");
                quote! { #ident }
            }
        }
    };
    if c.nullable || force_nullable {
        quote! { ::std::option::Option<#inner> }
    } else {
        inner
    }
}
