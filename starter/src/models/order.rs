//! The example child resource. Customers place orders through the custom
//! flow in `services/orders.rs` (an order needs a *published* product and
//! the customer's own account), so there is no generated create
//! (`no_create`). Managers work the generated admin at `/admin/orders`: the
//! list shows the product and customer by name (`#[ui(list)]` on the
//! relations), and pending orders get *Fulfill*/*Cancel* buttons (row
//! actions, gated by `can_act`).

use crate::models::product::Product;
use crate::models::user::User;
use crate::{
    ActionCx, AppResult, CurrentUser, Db, DbEnum, ModelHooks, chrono::NaiveDateTime, model, update,
};

#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderStatus {
    Pending,
    Fulfilled,
    Cancelled,
}

#[model(no_create, actions(fulfill, cancel), hooks)]
pub struct Order {
    pub id: i64,
    /// Read-only in the admin: an order never moves to another product or
    /// customer.
    #[orm(index, references(Product, on_delete = cascade))]
    #[ui(readonly)]
    pub product_id: i64,
    #[orm(relation = product_id)]
    #[ui(list)]
    pub product: Option<Product>,
    #[orm(index, references(User, on_delete = cascade))]
    #[ui(readonly)]
    pub user_id: i64,
    #[orm(relation = user_id)]
    #[ui(list)]
    pub user: Option<User>,
    #[orm(default = 1)]
    #[ui(list, min = 1, max = 99)]
    pub quantity: i64,
    #[ui(textarea)]
    pub note: Option<String>,
    #[orm(default = "pending")]
    #[ui(list, filter)]
    pub status: OrderStatus,
    #[orm(default = now)]
    #[ui(list, format = datetime)]
    pub created_at: NaiveDateTime,
}

impl Order {
    /// Row action: mark a pending order fulfilled.
    pub async fn fulfill(&self, cx: ActionCx<'_>) -> AppResult<()> {
        let id = self.id;
        update!(Order, cx.db, id == id; status = OrderStatus::Fulfilled).await?;
        Ok(())
    }

    /// Row action: cancel a pending order.
    pub async fn cancel(&self, cx: ActionCx<'_>) -> AppResult<()> {
        let id = self.id;
        update!(Order, cx.db, id == id; status = OrderStatus::Cancelled).await?;
        Ok(())
    }
}

impl ModelHooks for Order {
    /// Both actions only make sense while the order is pending.
    async fn can_act(&self, _action: &str, _db: &Db, _user: &CurrentUser) -> AppResult<bool> {
        Ok(self.status == OrderStatus::Pending)
    }
}
