//! The example resource. `#[model]` makes this struct the whole feature:
//! the table (via `fse migrate`), the ORM's typed queries, and the generated
//! admin CRUD at `/admin/products` (list with search + status filter,
//! create/edit forms, delete) guarded by `products.read`/`products.write`.
//!
//! The *public* catalog (`/products`, published rows only) is deliberately
//! not generated — it's hand-written in `services/products_public.rs` as the
//! canonical example of overriding: generation can't know that only
//! `published` rows are public.

use crate::{DbEnum, chrono::NaiveDateTime, model};

#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductStatus {
    Draft,
    Published,
    Archived,
}

#[model]
pub struct Product {
    pub id: i64,
    #[ui(list, search)]
    pub name: String,
    #[orm(unique)]
    #[ui(list)]
    pub slug: String,
    #[ui(textarea)]
    pub description: Option<String>,
    #[orm(default = 0.0)]
    #[ui(list)]
    pub price: f64,
    #[orm(default = "draft")]
    #[ui(list, filter)]
    pub status: ProductStatus,
    #[orm(default = now)]
    #[ui(list)]
    pub created_at: NaiveDateTime,
}
