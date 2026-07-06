//! A user placing an order for a product — a minimal example of a child
//! resource with its own moderation workflow.

use crate::{DbEnum, Table, chrono::NaiveDateTime};

/// An order starts `pending` and is moved to `fulfilled` (or `cancelled`)
/// by a manager from the product-manager orders tab; users can cancel their
/// own pending orders.
#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Pending,
    Fulfilled,
    Cancelled,
}

#[derive(Table, Debug, Clone)]
pub struct Order {
    pub id: i64,
    #[orm(index, references(Product, on_delete = cascade))]
    pub product_id: i64,
    #[orm(index, references(User, on_delete = cascade))]
    pub user_id: i64,
    #[orm(default = 1)]
    pub quantity: i64,
    pub note: Option<String>,
    #[orm(default = "pending")]
    pub status: OrderStatus,
    #[orm(default = now)]
    pub created_at: NaiveDateTime,
}
