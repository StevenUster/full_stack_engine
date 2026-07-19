//! The example child resource. The generated admin CRUD at `/admin/orders`
//! (guarded by `orders.read`/`orders.write`) covers moderation; the
//! user-facing flows that generation can't know — placing an order against a
//! published product, "my orders", cancelling your *own* pending order —
//! live in `services/orders.rs` as the example of custom logic beside a
//! generated model.

use crate::{DbEnum, chrono::NaiveDateTime, model};

#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Pending,
    Fulfilled,
    Cancelled,
}

#[model(no_create)]
pub struct Order {
    pub id: i64,
    #[orm(index, references(Product, on_delete = cascade))]
    pub product_id: i64,
    #[orm(index, references(User, on_delete = cascade))]
    pub user_id: i64,
    #[orm(default = 1)]
    #[ui(list)]
    pub quantity: i64,
    #[ui(textarea)]
    pub note: Option<String>,
    #[orm(default = "pending")]
    #[ui(list, filter)]
    pub status: OrderStatus,
    #[orm(default = now)]
    #[ui(list)]
    pub created_at: NaiveDateTime,
}
