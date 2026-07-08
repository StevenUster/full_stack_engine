//! Compiles the `find!` filter expression into a WHERE fragment plus bind
//! expressions. Column names are validated against the table at compile
//! time; every value stays a `?` placeholder.
//!
//! Grammar:
//! - `all` — no filter
//! - `col == expr`, `!=`, `<`, `<=`, `>`, `>=`
//! - `col.contains(e)` / `col.starts_with(e)` — LIKE, pattern built in SQL
//! - `col.contains_opt(e)` — `(? = '' OR col LIKE ...)`: empty string means
//!   "no filter" (the idiom for optional search boxes)
//! - `col.eq_opt(e)` (+ `ne/lt/lte/gt/gte_opt`) — `(? IS NULL OR col <op> ?)`:
//!   `None` means "no filter"
//! - `col.is_null()` / `col.is_not_null()`
//! - `&&`, `||`, parentheses

use fse_schema::{ColumnDef, TableDef};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::spanned::Spanned;

pub struct Filter {
    /// WHERE fragment without the `WHERE` keyword; empty for `all`.
    pub sql: String,
    /// `let __fb0 = <expr>;` hoists — sqlx macros borrow their arguments,
    /// so temporaries must become locals (and `_opt` binds twice).
    pub locals: Vec<TokenStream>,
    /// Bind names in placeholder order.
    pub args: Vec<syn::Ident>,
}

/// `qualifier`: when a query joins other tables via `include:`, a bare column
/// name is ambiguous, so every reference to the primary table's own columns
/// must be qualified (`{qualifier}.{col}`). `None` (the no-join case)
/// produces byte-identical SQL to before this parameter existed.
pub fn compile(expr: &syn::Expr, table: &TableDef, qualifier: Option<&str>) -> syn::Result<Filter> {
    if let syn::Expr::Path(p) = expr
        && p.path.is_ident("all")
    {
        return Ok(Filter { sql: String::new(), locals: Vec::new(), args: Vec::new() });
    }
    let mut filter = Filter { sql: String::new(), locals: Vec::new(), args: Vec::new() };
    let sql = walk(expr, table, qualifier, &mut filter)?;
    filter.sql = sql;
    Ok(filter)
}

fn qualified(name: &str, qualifier: Option<&str>) -> String {
    match qualifier {
        Some(q) => format!("{q}.{name}"),
        None => name.to_string(),
    }
}

fn walk(
    expr: &syn::Expr,
    table: &TableDef,
    qualifier: Option<&str>,
    out: &mut Filter,
) -> syn::Result<String> {
    match expr {
        syn::Expr::Paren(p) => walk(&p.expr, table, qualifier, out),
        syn::Expr::Binary(b) => match b.op {
            syn::BinOp::And(_) => {
                let left = walk(&b.left, table, qualifier, out)?;
                let right = walk(&b.right, table, qualifier, out)?;
                Ok(format!("({left} AND {right})"))
            }
            syn::BinOp::Or(_) => {
                let left = walk(&b.left, table, qualifier, out)?;
                let right = walk(&b.right, table, qualifier, out)?;
                Ok(format!("({left} OR {right})"))
            }
            syn::BinOp::Eq(_) => comparison(b, "=", table, qualifier, out),
            syn::BinOp::Ne(_) => comparison(b, "<>", table, qualifier, out),
            syn::BinOp::Lt(_) => comparison(b, "<", table, qualifier, out),
            syn::BinOp::Le(_) => comparison(b, "<=", table, qualifier, out),
            syn::BinOp::Gt(_) => comparison(b, ">", table, qualifier, out),
            syn::BinOp::Ge(_) => comparison(b, ">=", table, qualifier, out),
            _ => Err(syn::Error::new(b.op.span(), "unsupported operator in filter")),
        },
        syn::Expr::MethodCall(call) => method(call, table, qualifier, out),
        other => Err(syn::Error::new(
            other.span(),
            "expected a filter like `col == value`, `col.contains(v)` or `all`",
        )),
    }
}

fn comparison(
    b: &syn::ExprBinary,
    op: &str,
    table: &TableDef,
    qualifier: Option<&str>,
    out: &mut Filter,
) -> syn::Result<String> {
    let column = column_of(&b.left, table)?;
    let arg = bind(&b.right, out);
    Ok(format!("{} {op} {arg}", qualified(&column.name, qualifier)))
}

fn method(
    call: &syn::ExprMethodCall,
    table: &TableDef,
    qualifier: Option<&str>,
    out: &mut Filter,
) -> syn::Result<String> {
    let column = column_of(&call.receiver, table)?;
    let name = qualified(&column.name, qualifier);
    let method = call.method.to_string();

    let one_arg = || -> syn::Result<&syn::Expr> {
        if call.args.len() == 1 {
            Ok(&call.args[0])
        } else {
            Err(syn::Error::new(call.span(), format!("{method} takes exactly one argument")))
        }
    };

    match method.as_str() {
        "is_null" | "is_not_null" => {
            if !call.args.is_empty() {
                return Err(syn::Error::new(call.span(), format!("{method} takes no arguments")));
            }
            Ok(if method == "is_null" {
                format!("{name} IS NULL")
            } else {
                format!("{name} IS NOT NULL")
            })
        }
        "contains" => {
            let arg = bind_like(one_arg()?, false, out);
            Ok(format!("{name} LIKE '%' || {arg} || '%' ESCAPE '\\'"))
        }
        "starts_with" => {
            let arg = bind_like(one_arg()?, false, out);
            Ok(format!("{name} LIKE {arg} || '%' ESCAPE '\\'"))
        }
        "contains_opt" => {
            // Escaping never adds/removes emptiness, so the `= ''` "no
            // filter" test still sees the user's empty string.
            let arg = bind_like(one_arg()?, true, out);
            Ok(format!("({arg} = '' OR {name} LIKE '%' || {arg} || '%' ESCAPE '\\')"))
        }
        "eq_opt" | "ne_opt" | "lt_opt" | "lte_opt" | "gt_opt" | "gte_opt" => {
            let op = match method.as_str() {
                "eq_opt" => "=",
                "ne_opt" => "<>",
                "lt_opt" => "<",
                "lte_opt" => "<=",
                "gt_opt" => ">",
                _ => ">=",
            };
            let arg = bind_twice(one_arg()?, out);
            Ok(format!("({arg} IS NULL OR {name} {op} {arg})"))
        }
        other => Err(syn::Error::new(
            call.method.span(),
            format!(
                "unknown filter method `{other}`; expected contains, starts_with, \
                 contains_opt, eq_opt, ne_opt, lt_opt, lte_opt, gt_opt, gte_opt, \
                 is_null or is_not_null"
            ),
        )),
    }
}

/// Resolve a bare identifier to a column, rejecting json columns (their TEXT
/// representation is not meaningfully comparable).
pub fn column_of<'t>(expr: &syn::Expr, table: &'t TableDef) -> syn::Result<&'t ColumnDef> {
    let syn::Expr::Path(p) = expr else {
        return Err(syn::Error::new(expr.span(), "expected a column name"));
    };
    let Some(ident) = p.path.get_ident() else {
        return Err(syn::Error::new(expr.span(), "expected a column name"));
    };
    let column = table.column(&ident.to_string()).ok_or_else(|| {
        syn::Error::new(
            ident.span(),
            format!("no column `{ident}` on table `{}`", table.name),
        )
    })?;
    if column.json {
        return Err(syn::Error::new(
            ident.span(),
            format!("`{ident}` is a #[orm(json)] column and cannot be filtered on"),
        ));
    }
    Ok(column)
}

/// Hoist `expr` into a local and emit one `?`, returning the placeholder.
fn bind(expr: &syn::Expr, out: &mut Filter) -> String {
    let name = format_ident!("__fb{}", out.locals.len());
    out.locals.push(quote! { let #name = #expr; });
    out.args.push(name);
    "?".into()
}

/// Hoist once, bind twice (`_opt` operators test and use the same value).
/// Both placeholders bind the same local; the caller's format string uses
/// the returned `?` for each occurrence.
fn bind_twice(expr: &syn::Expr, out: &mut Filter) -> String {
    let name = format_ident!("__fb{}", out.locals.len());
    out.locals.push(quote! { let #name = #expr; });
    out.args.push(name.clone());
    out.args.push(name);
    "?".into()
}

/// Like [`bind`]/[`bind_twice`], but the value is a user-supplied LIKE search
/// term: `%`/`_`/`\` are escaped at runtime so they match literally (the SQL
/// side pairs the pattern with `ESCAPE '\'`). Untrusted input must never
/// widen a filter.
fn bind_like(expr: &syn::Expr, twice: bool, out: &mut Filter) -> String {
    let name = format_ident!("__fb{}", out.locals.len());
    out.locals.push(quote! { let #name = ::fse_orm::escape_like(#expr); });
    if twice {
        out.args.push(name.clone());
    }
    out.args.push(name);
    "?".into()
}
