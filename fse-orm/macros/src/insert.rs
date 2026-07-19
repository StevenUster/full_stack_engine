//! `insert!(Table, executor, col = value, ...)` — checked INSERT with
//! `RETURNING`. Expands to a literal-SQL `sqlx::query!`; awaiting the block
//! yields the full inserted row as `Table`.
//!
//! Columns you omit that are nullable or carry `#[orm(default = ...)]` are
//! left out of the INSERT list entirely — the DDL already gives them a real
//! SQL `DEFAULT`/implicit `NULL` (see `fse_schema::sql`), so SQLite fills
//! them in itself. The returned row is converted back to `Table` through the
//! same `codegen::build_fields` that `find!` uses, so a default's actual
//! value is never reproduced in Rust. Omitting a NOT NULL column with no
//! default is a compile error. The recognized auto-increment surrogate key
//! (a lone `id: i64` primary key) can never be assigned explicitly.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Token;
use syn::parse::{Parse, ParseStream};

use crate::{codegen, lookup};

pub struct InsertInput {
    table_ident: syn::Ident,
    db: syn::Expr,
    assignments: Vec<(syn::Ident, syn::Expr)>,
}

impl Parse for InsertInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let table_ident: syn::Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let db: syn::Expr = input.parse()?;

        let mut assignments = Vec::new();
        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break; // trailing comma
            }
            let column: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: syn::Expr = input.parse()?;
            assignments.push((column, value));
        }
        Ok(Self {
            table_ident,
            db,
            assignments,
        })
    }
}

pub fn expand(input: &InsertInput) -> syn::Result<TokenStream> {
    let table = lookup::load_table(&input.table_ident.to_string(), input.table_ident.span())?;
    let auto_id_col = table.auto_id().then_some("id");

    // Values are evaluated in the caller scope as owned locals (no `?`, no
    // capture of surrounding variables by the async block); the fallible
    // json/enum conversions then run *inside* the async block, where `?`
    // targets the query's Result — same split as `update!`.
    let mut raw_locals: Vec<TokenStream> = Vec::new();
    let mut conv_locals: Vec<TokenStream> = Vec::new();
    let mut args: Vec<syn::Ident> = Vec::new();
    let mut col_names: Vec<String> = Vec::new();
    for (i, (column_ident, value)) in input.assignments.iter().enumerate() {
        let name = column_ident.to_string();
        let Some(column) = table.column(&name) else {
            return Err(syn::Error::new(
                column_ident.span(),
                format!("no column `{name}` on table `{}`", table.name),
            ));
        };
        if Some(name.as_str()) == auto_id_col {
            return Err(syn::Error::new(
                column_ident.span(),
                format!(
                    "`{name}` is an auto-increment primary key and cannot be inserted explicitly"
                ),
            ));
        }
        if col_names.contains(&name) {
            return Err(syn::Error::new(
                column_ident.span(),
                format!("column `{name}` assigned more than once"),
            ));
        }

        let raw = format_ident!("__raw{i}");
        let out = format_ident!("__ins{i}");
        raw_locals.push(quote! { let #raw = #value; });

        if column.json {
            conv_locals.push(if column.nullable {
                quote! { let #out = ::fse_orm::opt_to_json_string(#raw.as_ref())?; }
            } else {
                quote! { let #out = ::fse_orm::to_json_string(&#raw)?; }
            });
            args.push(out);
        } else if column.is_enum {
            conv_locals.push(if column.nullable {
                quote! { let #out = #raw.as_ref().map(|v| v.as_str()); }
            } else {
                quote! { let #out = #raw.as_str(); }
            });
            args.push(out);
        } else {
            args.push(raw);
        }
        col_names.push(name);
    }

    // Every column not assigned above must be able to fill itself in:
    // nullable (implicit NULL) or `#[orm(default = ...)]` (a real SQL
    // DEFAULT). Anything else omitted is a missing required column.
    for column in &table.columns {
        if Some(column.name.as_str()) == auto_id_col || col_names.contains(&column.name) {
            continue;
        }
        if !column.nullable && column.default.is_none() {
            return Err(syn::Error::new(
                input.table_ident.span(),
                format!(
                    "insert! is missing required column `{}` on `{}` (not nullable, no default)",
                    column.name, table.name
                ),
            ));
        }
    }

    let select_list = codegen::select_list(&table.columns);
    let quoted_table = fse_schema::sql::quote_ident(&table.name);
    let sql = if col_names.is_empty() {
        format!("INSERT INTO {quoted_table} DEFAULT VALUES RETURNING {select_list}")
    } else {
        let marks = vec!["?"; col_names.len()].join(", ");
        format!(
            "INSERT INTO {quoted_table} ({}) VALUES ({marks}) RETURNING {select_list}",
            col_names
                .iter()
                .map(|c| fse_schema::sql::quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let build_fields = codegen::build_fields(&table.columns);
    let relation_inits: Vec<TokenStream> = table
        .relations
        .iter()
        .map(|r| {
            let id = format_ident!("{}", r.field);
            quote! { #id: None }
        })
        .collect();

    let db = &input.db;
    let table_ident = &input.table_ident;
    Ok(quote! {
        {
            let __db = #db;
            // Reference the table type so an import used only through this
            // macro still counts as used and resolves to a real Table struct.
            let _ = ::core::marker::PhantomData::<#table_ident>;
            #(#raw_locals)*
            async move {
                #(#conv_locals)*
                let r = ::sqlx::query!(#sql #(, #args)*).fetch_one(__db).await?;
                Ok::<_, ::sqlx::Error>(#table_ident { #(#build_fields,)* #(#relation_inits),* })
            }
        }
    })
}
