//! The example resource — one struct is the whole feature:
//!
//! - the table (via `fse migrate`) and the ORM's typed queries,
//! - the admin CRUD at `/admin/products` (search, status filter, forms with
//!   validation), guarded by `products.read`/`products.write`,
//! - the public catalog at `/products` + `/products/{slug}` (`public_read`)
//!   and the JSON API at `/api/products` (`api`) — both limited to
//!   *published* rows by `public_scope` below.
//!
//! The catalog pages are styled by `themes/starter/src/pages/products/` (a template
//! named after the model wins over the generic one) — no Rust for them.

use crate::{Cond, DbEnum, ModelHooks, chrono::NaiveDateTime, model};

#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductStatus {
    Draft,
    Published,
    Archived,
}

#[model(public_read = slug, api, hooks)]
pub struct Product {
    pub id: i64,
    #[ui(list, search, max = 120)]
    pub name: String,
    /// Left empty, it is derived from the name (and kept on edit).
    #[orm(unique)]
    #[ui(list, slug_from = name)]
    pub slug: String,
    #[ui(textarea)]
    pub description: Option<String>,
    #[orm(default = 0.0)]
    #[ui(list, min = 0, format = currency)]
    pub price: f64,
    #[orm(default = "draft")]
    #[ui(list, filter)]
    pub status: ProductStatus,
    #[orm(default = now)]
    #[ui(list, format = date)]
    pub created_at: NaiveDateTime,
}

impl ModelHooks for Product {
    /// Only published products exist for the public pages and the API.
    fn public_scope() -> Option<Cond> {
        Some(Product::STATUS.eq(ProductStatus::Published))
    }
}
