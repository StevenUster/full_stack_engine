//! Hand-written routes — **overrides and custom flows only**. Everything
//! CRUD-shaped comes from the `#[model]` structs in `src/models/` (mounted
//! by `.models::<AppRole>()`) and the auth flows from the framework's auth
//! module. Registration order is the override mechanism: these routes mount
//! first, so a same-path route here beats a module or generated one.

use crate::web;

pub use full_stack_engine::prelude::RenderTplExt;

pub mod api;
pub mod index;
pub mod orders;
pub mod products_public;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(index::index);

    // Public catalog: the override example — published products only.
    cfg.service(products_public::get_public_products);
    cfg.service(products_public::get_public_product_detail);

    // User-facing order flows beside the generated /admin/orders CRUD.
    cfg.service(orders::post_place_order);
    cfg.service(orders::get_my_orders);
    cfg.service(orders::post_cancel_my_order);

    // Public JSON API + OpenAPI/Swagger (exposes published data only).
    cfg.service(api::get_docs);
    cfg.service(api::get_openapi_spec);
    cfg.service(api::get_products);
    cfg.service(api::get_product_detail);
}
