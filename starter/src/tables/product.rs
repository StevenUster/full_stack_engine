//! Example manageable resource: a catalog product with a simple publish
//! lifecycle. Meant as a template for "the thing your app actually manages"
//! — copy this file, rename `Product`/`ProductStatus`, adjust the fields,
//! and run `fse migrate`.

use crate::{DbEnum, Table, chrono::NaiveDateTime};

/// `draft` products are not yet public, `published` ones show in the
/// catalog, `archived` ones are hidden again (e.g. discontinued) but kept
/// for historical orders. Stored as TEXT with a CHECK constraint.
#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductStatus {
    Draft,
    Published,
    Archived,
}

#[derive(Table, Debug, Clone)]
pub struct Product {
    pub id: i64,
    pub name: String,
    #[orm(unique)]
    pub slug: String,
    pub description: Option<String>,
    #[orm(default = 0.0)]
    pub price: f64,
    #[orm(default = "draft")]
    pub status: ProductStatus,
    #[orm(default = now)]
    pub created_at: NaiveDateTime,
}
