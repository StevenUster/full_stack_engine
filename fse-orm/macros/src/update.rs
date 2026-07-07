//! `update!(Table, executor, <filter>; col = value, ...)` — checked partial
//! update. Expands to a literal-SQL `sqlx::query!`; awaiting the block
//! yields the number of affected rows.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Token;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;

use crate::{filter, lookup};

pub struct UpdateInput {
    table_ident: syn::Ident,
    db: syn::Expr,
    filter: syn::Expr,
    assignments: Vec<(syn::Ident, syn::Expr)>,
}

impl Parse for UpdateInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let table_ident: syn::Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let db: syn::Expr = input.parse()?;
        input.parse::<Token![,]>()?;
        let filter: syn::Expr = input.parse()?;
        input.parse::<Token![;]>()?;

        let mut assignments = Vec::new();
        loop {
            let column: syn::Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            let value: syn::Expr = input.parse()?;
            assignments.push((column, value));
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
                if input.is_empty() {
                    break; // trailing comma
                }
            } else {
                break;
            }
        }
        Ok(Self { table_ident, db, filter, assignments })
    }
}

pub fn expand(input: &UpdateInput) -> syn::Result<TokenStream> {
    let table = lookup::load_table(&input.table_ident.to_string(), input.table_ident.span())?;

    // SET values are evaluated in the caller scope as owned locals (no `?`,
    // no capture of surrounding variables by the async block); the fallible
    // json/enum conversions then run *inside* the async block, where `?`
    // targets the query's Result.
    let mut raw_locals: Vec<TokenStream> = Vec::new();
    let mut conv_locals: Vec<TokenStream> = Vec::new();
    let mut args: Vec<syn::Ident> = Vec::new();
    let mut set_items: Vec<String> = Vec::new();
    for (i, (column_ident, value)) in input.assignments.iter().enumerate() {
        let name = column_ident.to_string();
        let Some(column) = table.column(&name) else {
            return Err(syn::Error::new(
                column_ident.span(),
                format!("no column `{name}` on table `{}`", table.name),
            ));
        };
        if column.primary_key {
            return Err(syn::Error::new(
                column_ident.span(),
                format!("`{name}` is a primary key and cannot be updated"),
            ));
        }

        let raw = format_ident!("__raw{i}");
        let out = format_ident!("__set{i}");
        raw_locals.push(quote! { let #raw = #value; });

        // json/enum values need the same conversions the derive applies.
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
            // Plain columns bind the raw local directly.
            args.push(raw);
        }
        set_items.push(format!("{name} = ?"));
    }
    if set_items.is_empty() {
        return Err(syn::Error::new(input.filter.span(), "update! needs at least one `col = value`"));
    }

    let compiled = filter::compile(&input.filter, &table, None)?;
    let where_clause = if compiled.sql.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", compiled.sql)
    };
    let sql = format!("UPDATE {} SET {}{where_clause}", table.name, set_items.join(", "));

    let db = &input.db;
    let table_ident = &input.table_ident;
    let filter_locals = &compiled.locals;
    let filter_args = &compiled.args;
    Ok(quote! {
        {
            let __db = #db;
            // Reference the table type so an import used only through this
            // macro still counts as used and resolves to a real Table struct.
            let _ = ::core::marker::PhantomData::<#table_ident>;
            #(#raw_locals)*
            #(#filter_locals)*
            async move {
                #(#conv_locals)*
                let result = ::sqlx::query!(#sql #(, #args)* #(, #filter_args)*)
                    .execute(__db)
                    .await?;
                Ok::<_, ::sqlx::Error>(result.rows_affected())
            }
        }
    })
}
