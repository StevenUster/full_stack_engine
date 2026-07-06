//! Proc macros for the fse ORM.

use proc_macro::TokenStream;

mod codegen;
mod db_enum;
mod filter;
mod find;
mod lookup;
mod table;
mod update;

/// Marks a struct as a database table and generates compile-time-checked
/// CRUD: `fetch`, `fetch_all`, `fetch_by_<unique>`, `count`, `update`,
/// `delete`, plus an `InsertX` companion struct whose `insert` returns the
/// full row. All generated SQL is literal, so `sqlx` verifies it against the
/// real database schema at compile time.
#[proc_macro_derive(Table, attributes(orm))]
pub fn derive_table(input: TokenStream) -> TokenStream {
    let item = syn::parse_macro_input!(input as syn::ItemStruct);
    table::expand(&item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Marks a fieldless enum as a TEXT-stored database value (snake_case of the
/// variant name). Generates `as_str`/`FromStr`/`Display`/`VARIANTS`, the
/// sqlx `Type`/`Encode`/`Decode` impls and string-based serde impls — do not
/// also derive `Serialize`/`Deserialize` on it.
#[proc_macro_derive(DbEnum, attributes(orm))]
pub fn derive_db_enum(input: TokenStream) -> TokenStream {
    let item = syn::parse_macro_input!(input as syn::ItemEnum);
    db_enum::expand(&item)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_query(input: TokenStream, mode: find::Mode) -> TokenStream {
    let parsed = syn::parse_macro_input!(input as find::QueryInput);
    find::expand(&parsed, &mode)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// `find!(Table, executor, filter [, order_by: ...][, limit: n][, offset: n])`
/// → awaitable, yields `sqlx::Result<Vec<Table>>`. Filter grammar: `all`,
/// comparisons, `contains`/`starts_with`, `_opt` variants (empty string /
/// `None` mean "no filter"), `is_null`/`is_not_null`, `&&`, `||`.
#[proc_macro]
pub fn find(input: TokenStream) -> TokenStream {
    expand_query(input, find::Mode::All)
}

/// `find_one!(Table, executor, filter)` → `sqlx::Result<Option<Table>>`.
#[proc_macro]
pub fn find_one(input: TokenStream) -> TokenStream {
    expand_query(input, find::Mode::One)
}

/// `find_page!(Table, executor, filter [, order_by: ...], page: p, per_page: n)`
/// → `sqlx::Result<Page<Table>>`: one filter definition drives both the
/// COUNT and the LIMIT/OFFSET SELECT. Runs two queries, so pass a pool.
#[proc_macro]
pub fn find_page(input: TokenStream) -> TokenStream {
    expand_query(input, find::Mode::Page)
}

/// `count!(Table, executor, filter)` → `sqlx::Result<i64>`.
#[proc_macro]
pub fn count(input: TokenStream) -> TokenStream {
    expand_query(input, find::Mode::Count)
}

/// `delete!(Table, executor, filter)` → `sqlx::Result<u64>` (rows deleted).
/// `all` is accepted — it deletes every row, so type it deliberately.
#[proc_macro]
pub fn delete(input: TokenStream) -> TokenStream {
    expand_query(input, find::Mode::Delete)
}

/// `update!(Table, executor, filter; col = value, ...)` → `sqlx::Result<u64>`
/// (rows updated). json/enum columns get the same conversions the derive
/// applies.
#[proc_macro]
pub fn update(input: TokenStream) -> TokenStream {
    let parsed = syn::parse_macro_input!(input as update::UpdateInput);
    update::expand(&parsed)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
