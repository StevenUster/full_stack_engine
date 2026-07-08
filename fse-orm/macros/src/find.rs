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

use crate::{codegen, filter, lookup, relation};

pub struct QueryInput {
    pub table_ident: syn::Ident,
    pub db: syn::Expr,
    pub filter: syn::Expr,
    pub order_by: Vec<OrderItem>,
    pub limit: Option<syn::Expr>,
    pub offset: Option<syn::Expr>,
    pub page: Option<syn::Expr>,
    pub per_page: Option<syn::Expr>,
    /// `include: [run, donor]` — eager-load these relation fields via a real
    /// SQL JOIN. Only meaningful for `find!`/`find_one!` (see
    /// `forbid_unused_options`).
    pub include: Vec<syn::Ident>,
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
            include: Vec::new(),
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
                "include" => parsed.include = parse_include_list(input)?,
                other => {
                    return Err(syn::Error::new(
                        keyword.span(),
                        format!(
                            "unknown option `{other}`; expected order_by, limit, offset, page, \
                             per_page or include"
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

/// `include: [run, donor]` — a bracketed, comma-separated list of relation
/// field names.
fn parse_include_list(input: ParseStream) -> syn::Result<Vec<syn::Ident>> {
    let content;
    syn::bracketed!(content in input);
    let items = content.parse_terminated(syn::Ident::parse, Token![,])?;
    Ok(items.into_iter().collect())
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
    forbid_unused_options(input, mode)?;

    let includes = relation::resolve(&table, &input.include)?;
    let table_name = &table.name;

    // A join makes a bare column name ambiguous between the primary table and
    // a joined one, so once anything is joined in, the primary table's own
    // SELECT items, WHERE clause and ORDER BY are all qualified
    // (`{table}.{col}`). With no `include:` this stays `None`, so every
    // string generated below is byte-identical to before `include:` existed
    // — the overwhelming majority of call sites never churn the offline
    // query cache from this feature.
    let qualifier: Option<&str> = if includes.is_empty() { None } else { Some(table_name.as_str()) };

    let filter = filter::compile(&input.filter, &table, qualifier)?;
    let where_clause = if filter.sql.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filter.sql)
    };
    let order_clause = order_clause(input, &table, qualifier)?;

    let table_ident = &input.table_ident;
    let db = &input.db;
    let locals = &filter.locals;
    let args = &filter.args;
    let own_select = match qualifier {
        Some(q) => codegen::select_list_qualified(&table.columns, q),
        None => codegen::select_list(&table.columns),
    };
    let joins: String =
        includes.iter().map(|inc| format!(" {}", relation::join_clause(table_name, inc))).collect();
    let select_list = {
        let mut items = vec![own_select];
        items.extend(includes.iter().flat_map(relation::select_items));
        items.join(", ")
    };
    let build_fields = codegen::build_fields(&table.columns);
    // Every relation field on the struct needs an initializer: the ones named
    // in `include:` are built by the owning struct's own hidden
    // `__fse_relation_*` helper (see relation.rs — it, not this call site,
    // is where the target type name resolves), the rest default to `None`
    // (same as an ordinary `fetch`/`fetch_all` that never touches relations).
    let relation_fields: Vec<TokenStream> = table
        .relations
        .iter()
        .map(|r| match includes.iter().find(|inc| inc.relation.field == r.field) {
            Some(inc) => {
                let id = format_ident!("{}", r.field);
                let call = relation::call_expr(table_ident, inc);
                quote! { #id: #call }
            }
            None => {
                let id = format_ident!("{}", r.field);
                quote! { #id: None }
            }
        })
        .collect();
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
                format!("SELECT {select_list} FROM {table_name}{joins}{where_clause}{order_clause}");
            let mut extra_locals = Vec::new();
            let mut extra_args: Vec<syn::Ident> = Vec::new();
            match (&input.limit, &input.offset) {
                (Some(limit), offset) => {
                    sql.push_str(" LIMIT ?");
                    // Clamp: SQLite reads a negative LIMIT as "unlimited", so
                    // an unvalidated value must not turn a bounded query into
                    // a full-table read.
                    extra_locals.push(quote! { let __limit: i64 = <i64>::max(#limit, 0); });
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
                            Ok(#table_ident { #(#build_fields,)* #(#relation_fields),* })
                        })
                        .collect::<::sqlx::Result<::std::vec::Vec<#table_ident>>>()
                },
            )
        }
        Mode::One => {
            let sql = format!(
                "SELECT {select_list} FROM {table_name}{joins}{where_clause}{order_clause} LIMIT 1"
            );
            (
                quote! {},
                quote! {
                    let row = ::sqlx::query!(#sql #(, #args)*).fetch_optional(__db).await?;
                    let row = match row {
                        Some(r) => Some(#table_ident { #(#build_fields,)* #(#relation_fields),* }),
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
                "SELECT {select_list} FROM {table_name}{joins}{where_clause}{order_clause} LIMIT ? OFFSET ?"
            );
            (
                quote! {
                    // Clamped like `page`: SQLite reads a negative LIMIT as
                    // "unlimited", so a hostile per_page must not dump the
                    // whole table.
                    let __per_page: i64 = <i64>::max(#per_page, 1);
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
                            Ok(#table_ident { #(#build_fields,)* #(#relation_fields),* })
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

fn order_clause(input: &QueryInput, table: &TableDef, qualifier: Option<&str>) -> syn::Result<String> {
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
        let name = match qualifier {
            Some(q) => format!("{q}.{name}"),
            None => name,
        };
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
            if !input.include.is_empty() {
                return complain("include (not yet supported on find_page!)");
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
            if !input.include.is_empty() {
                return complain("include");
            }
        }
    }
    Ok(())
}
