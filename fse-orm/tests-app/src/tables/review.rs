//! Exercises the LEFT JOIN side of `include:`: `product_id` is nullable, so
//! `product` is absent for reviews left behind after their product is
//! deleted (`on_delete = set_null`) — the row survives, the relation doesn't.

use fse_orm::Table;

use crate::tables::product::Product;

#[derive(Table, Debug, Clone)]
pub struct Review {
    pub id: i64,
    #[orm(references(Product, on_delete = set_null))]
    pub product_id: Option<i64>,
    #[orm(relation = product_id)]
    pub product: Option<Product>,
    pub rating: i64,
    pub comment: Option<String>,
}
