//! `find!` / `find_one!` / `find_page!` / `count!` / `delete!` — checked
//! query macros. Shape: `find!(Table, executor, filter [, kwargs])`. Each
//! expands to an `async move` block whose body is a literal-SQL
//! `sqlx::query!` call, so sqlx verifies it at compile time; awaiting the
//! block yields the result.
//!
//! The executor expression is evaluated *outside* the async block: a bare
//! `&db` inside `async move` would capture the pool itself by value.

use fse_schema::TableDef;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Token;
use syn::parse::{Parse, ParseStream};

use crate::{codegen, filter, lookup};

pub struct QueryInput {
    pub table_ident: syn::Ident,
    pub db: syn::Expr,
    pub filter: syn::Expr,
    pub order_by: Vec<OrderItem>,
    pub limit: Option<syn::Expr>,
    pub offset: Option<syn::Expr>,
    pub page: Option<syn::Expr>,
    pub per_page: Option<syn::Expr>,
}

pub struct OrderItem {
    pub column: syn::Ident,
    pub descending: bool,
}

impl Parse for QueryInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let table_ident: syn::Ident = input.parse()?;
        input.parse::<Token![,]>()?;
        let db: syn::Expr = input.parse()?;
        input.parse::<Token![,]>()?;
        let filter: syn::Expr = input.parse()?;

        let mut parsed = QueryInput {
            table_ident,
            db,
            filter,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            page: None,
            per_page: None,
        };

        while input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break; // trailing comma
            }
            let keyword: syn::Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            match keyword.to_string().as_str() {
                "order_by" => parsed.order_by = parse_order_items(input)?,
                "limit" => parsed.limit = Some(input.parse()?),
                "offset" => parsed.offset = Some(input.parse()?),
                "page" => parsed.page = Some(input.parse()?),
                "per_page" => parsed.per_page = Some(input.parse()?),
                other => {
                    return Err(syn::Error::new(
                        keyword.span(),
                        format!(
                            "unknown option `{other}`; expected order_by, limit, offset, page or per_page"
                        ),
                    ));
                }
            }
        }
        Ok(parsed)
    }
}

/// `created_at.desc(), name` — items until the stream ends or the next
/// `keyword:` begins.
fn parse_order_items(input: ParseStream) -> syn::Result<Vec<OrderItem>> {
    let mut items = Vec::new();
    loop {
        let column: syn::Ident = input.parse()?;
        let mut descending = false;
        if input.peek(Token![.]) {
            input.parse::<Token![.]>()?;
            let direction: syn::Ident = input.parse()?;
            let parens;
            syn::parenthesized!(parens in input);
            if !parens.is_empty() {
                return Err(syn::Error::new(direction.span(), "asc()/desc() take no arguments"));
            }
            descending = match direction.to_string().as_str() {
                "asc" => false,
                "desc" => true,
                other => {
                    return Err(syn::Error::new(
                        direction.span(),
                        format!("expected asc() or desc(), got `{other}`"),
                    ));
                }
            };
        }
        items.push(OrderItem { column, descending });

        // Another order item only if the next tokens are not `keyword:`.
        if input.peek(Token![,]) && input.peek2(syn::Ident) && !input.peek3(Token![:]) {
            input.parse::<Token![,]>()?;
            continue;
        }
        return Ok(items);
    }
}

pub enum Mode {
    All,
    One,
    Page,
    Count,
    Delete,
}

pub fn expand(input: &QueryInput, mode: &Mode) -> syn::Result<TokenStream> {
    let table = lookup::load_table(&input.table_ident.to_string(), input.table_ident.span())?;
    let filter = filter::compile(&input.filter, &table)?;
    let where_clause = if filter.sql.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filter.sql)
    };
    let order_clause = order_clause(input, &table)?;
    forbid_unused_options(input, mode)?;

    let table_ident = &input.table_ident;
    let db = &input.db;
    let locals = &filter.locals;
    let args = &filter.args;
    let select_list = codegen::select_list(&table.columns);
    let build_fields = codegen::build_fields(&table.columns);
    let table_name = &table.name;
    let count_sql =
        format!("SELECT COUNT(*) as \"count!: i64\" FROM {table_name}{where_clause}");

    // `setup` is evaluated in the surrounding scope, `body` inside
    // `async move`. Bind expressions must be hoisted into `setup` (like
    // sqlx, arguments are evaluated at the call site): evaluating them
    // inside the block would make `async move` capture surrounding locals
    // by value.
    let (setup, body) = match mode {
        Mode::All => {
            let mut sql =
                format!("SELECT {select_list} FROM {table_name}{where_clause}{order_clause}");
            let mut extra_locals = Vec::new();
            let mut extra_args: Vec<syn::Ident> = Vec::new();
            match (&input.limit, &input.offset) {
                (Some(limit), offset) => {
                    sql.push_str(" LIMIT ?");
                    extra_locals.push(quote! { let __limit: i64 = #limit; });
                    extra_args.push(format_ident!("__limit"));
                    if let Some(offset) = offset {
                        sql.push_str(" OFFSET ?");
                        extra_locals.push(quote! { let __offset: i64 = #offset; });
                        extra_args.push(format_ident!("__offset"));
                    }
                }
                (None, Some(offset)) => {
                    // SQLite requires LIMIT before OFFSET; -1 means unlimited.
                    sql.push_str(" LIMIT -1 OFFSET ?");
                    extra_locals.push(quote! { let __offset: i64 = #offset; });
                    extra_args.push(format_ident!("__offset"));
                }
                (None, None) => {}
            }
            (
                quote! { #(#extra_locals)* },
                quote! {
                    let rows = ::sqlx::query!(#sql #(, #args)* #(, #extra_args)*)
                        .fetch_all(__db)
                        .await?;
                    rows.into_iter()
                        .map(|r| -> ::sqlx::Result<#table_ident> {
                            Ok(#table_ident { #(#build_fields),* })
                        })
                        .collect::<::sqlx::Result<::std::vec::Vec<#table_ident>>>()
                },
            )
        }
        Mode::One => {
            let sql = format!(
                "SELECT {select_list} FROM {table_name}{where_clause}{order_clause} LIMIT 1"
            );
            (
                quote! {},
                quote! {
                    let row = ::sqlx::query!(#sql #(, #args)*).fetch_optional(__db).await?;
                    let row = match row {
                        Some(r) => Some(#table_ident { #(#build_fields),* }),
                        None => None,
                    };
                    Ok::<_, ::sqlx::Error>(row)
                },
            )
        }
        Mode::Page => {
            let (Some(page), Some(per_page)) = (&input.page, &input.per_page) else {
                return Err(syn::Error::new(
                    input.table_ident.span(),
                    "find_page! needs `page:` and `per_page:`",
                ));
            };
            let sql = format!(
                "SELECT {select_list} FROM {table_name}{where_clause}{order_clause} LIMIT ? OFFSET ?"
            );
            (
                quote! {
                    let __per_page: i64 = #per_page;
                    let __page: i64 = <i64>::max(#page, 1);
                    let __offset: i64 = (__page - 1) * __per_page;
                },
                quote! {
                    let total = ::sqlx::query_scalar!(#count_sql #(, #args)*)
                        .fetch_one(__db)
                        .await?;
                    let rows = ::sqlx::query!(#sql #(, #args)*, __per_page, __offset)
                        .fetch_all(__db)
                        .await?;
                    let rows = rows
                        .into_iter()
                        .map(|r| -> ::sqlx::Result<#table_ident> {
                            Ok(#table_ident { #(#build_fields),* })
                        })
                        .collect::<::sqlx::Result<::std::vec::Vec<#table_ident>>>()?;
                    Ok::<_, ::sqlx::Error>(::fse_orm::Page { rows, total })
                },
            )
        }
        Mode::Count => (
            quote! {},
            quote! {
                ::sqlx::query_scalar!(#count_sql #(, #args)*).fetch_one(__db).await
            },
        ),
        Mode::Delete => {
            let sql = format!("DELETE FROM {table_name}{where_clause}");
            (
                quote! {},
                quote! {
                    let result = ::sqlx::query!(#sql #(, #args)*).execute(__db).await?;
                    Ok::<_, ::sqlx::Error>(result.rows_affected())
                },
            )
        }
    };

    Ok(quote! {
        {
            let __db = #db;
            // Reference the table type so an import used only through this
            // macro (count!/delete!, which don't otherwise name the type)
            // still counts as used and resolves to a real Table struct.
            let _ = ::core::marker::PhantomData::<#table_ident>;
            #(#locals)*
            #setup
            async move { #body }
        }
    })
}

fn order_clause(input: &QueryInput, table: &TableDef) -> syn::Result<String> {
    if input.order_by.is_empty() {
        return Ok(String::new());
    }
    let mut items = Vec::new();
    for item in &input.order_by {
        let name = item.column.to_string();
        if table.column(&name).is_none() {
            return Err(syn::Error::new(
                item.column.span(),
                format!("no column `{name}` on table `{}`", table.name),
            ));
        }
        items.push(if item.descending {
            format!("{name} DESC")
        } else {
            format!("{name} ASC")
        });
    }
    Ok(format!(" ORDER BY {}", items.join(", ")))
}

fn forbid_unused_options(input: &QueryInput, mode: &Mode) -> syn::Result<()> {
    let complain = |what: &str| {
        Err(syn::Error::new(
            input.table_ident.span(),
            format!("{what} does not apply to this macro"),
        ))
    };
    match mode {
        Mode::All => {
            if input.page.is_some() || input.per_page.is_some() {
                return complain("page/per_page (use find_page!)");
            }
        }
        Mode::One => {
            if input.limit.is_some()
                || input.offset.is_some()
                || input.page.is_some()
                || input.per_page.is_some()
            {
                return complain("limit/offset/page/per_page");
            }
        }
        Mode::Page => {
            if input.limit.is_some() || input.offset.is_some() {
                return complain("limit/offset (use page/per_page)");
            }
        }
        Mode::Count | Mode::Delete => {
            if !input.order_by.is_empty()
                || input.limit.is_some()
                || input.offset.is_some()
                || input.page.is_some()
                || input.per_page.is_some()
            {
                return complain("order_by/limit/offset/page/per_page");
            }
        }
    }
    Ok(())
}
