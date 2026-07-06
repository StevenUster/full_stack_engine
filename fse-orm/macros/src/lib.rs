//! Proc macros for the fse ORM. `find!`/`update!` (the checked filter
//! macros) land in build-order step 4.

use proc_macro::TokenStream;

mod db_enum;
mod table;

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
