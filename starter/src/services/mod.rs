//! Hand-written routes — **overrides and custom flows only**. Everything
//! CRUD-shaped comes from the `#[model]` structs in `src/models/` (mounted
//! by `.models::<AppRole>()`) and the auth flows from the framework's auth
//! module. Registration order is the override mechanism: these routes mount
//! first, so a same-path route here beats a module or generated one.

use crate::web;

pub use full_stack_engine::prelude::RenderTplExt;

pub mod index;
pub mod orders;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(index::index);

    // The customer's order flow — what generation can't know: an order
    // needs a published product and belongs to the signed-in customer.
    // Managers use the generated /admin/orders (with Fulfill/Cancel).
    cfg.service(orders::post_place_order);
    cfg.service(orders::get_my_orders);
    cfg.service(orders::post_cancel_my_order);
}
