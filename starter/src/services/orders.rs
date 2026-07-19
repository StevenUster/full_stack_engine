//! Override example #2: user-facing order flows next to a generated model.
//! Moderation (list/filter/fulfil/cancel/delete) is the generated CRUD at
//! `/admin/orders`; what lives here is what generation can't know — placing
//! an order against a *published* product, "my orders", and cancelling only
//! your *own pending* order (the ownership check is in the update filter).
//!
//! Related rows here are fetched with a second query over the collected ids
//! (`Col::in_` on the dynamic builder) and stitched in Rust, rather than
//! `include:` — a page-sized list only needs one extra indexed query either
//! way.

use std::collections::{HashMap, HashSet};

use crate::{
    AppData, AppError, AppResult, AppRole, AuthUser, Deserialize, Serialize,
    actix_web::{HttpResponse, get, post, web},
    find, find_one, insert, update,
};

use crate::models::order::{Order, OrderStatus};
use crate::models::product::{Product, ProductStatus};

#[derive(Deserialize)]
pub struct PlaceOrderForm {
    pub quantity: i64,
    pub note: Option<String>,
}

/// `POST /products/{slug}/order` — a signed-in user orders a product.
#[post("/products/{slug}/order")]
pub async fn post_place_order(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<String>,
    form: web::Form<PlaceOrderForm>,
) -> AppResult {
    let slug = path.into_inner();

    let product = find_one!(
        Product,
        &data.db,
        slug == slug.as_str() && status == ProductStatus::Published
    )
    .await?
    .ok_or_else(|| AppError::NotFound("product".to_string()))?;

    insert!(
        Order,
        &data.db,
        product_id = product.id,
        user_id = user.claims.sub,
        quantity = form.quantity.max(1),
        note = form.note.clone()
    )
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", format!("/products/{slug}?ordered=1")))
        .finish())
}

#[derive(Serialize)]
struct MyOrderRow {
    id: i64,
    quantity: i64,
    note: String,
    status: &'static str,
    created_at: String,
    product_name: String,
    product_link: String,
}

/// `GET /my-orders` — the signed-in user's own orders.
#[get("/my-orders")]
pub async fn get_my_orders(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
) -> AppResult {
    use super::RenderTplExt;

    let orders = find!(
        Order,
        &data.db,
        user_id == user.claims.sub,
        order_by: created_at.desc()
    )
    .await?;

    let products = products_by_id(&data, &orders).await?;

    let rows: Vec<MyOrderRow> = orders
        .into_iter()
        .filter_map(|o| {
            let product = products.get(&o.product_id)?;
            Some(MyOrderRow {
                id: o.id,
                quantity: o.quantity,
                note: o.note.unwrap_or_default(),
                status: o.status.as_str(),
                created_at: o.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                product_name: product.name.clone(),
                product_link: format!("/products/{}", product.slug),
            })
        })
        .collect();

    Ok(req.render_tpl("my-orders", &crate::json!({ "rows": rows })).await)
}

/// The products referenced by these orders, keyed by id.
async fn products_by_id(
    data: &web::Data<AppData>,
    orders: &[Order],
) -> Result<HashMap<i64, Product>, AppError> {
    let ids: HashSet<i64> = orders.iter().map(|o| o.product_id).collect();
    Ok(Product::find()
        .filter(Product::ID.in_(ids))
        .fetch_all(&data.db)
        .await?
        .into_iter()
        .map(|p| (p.id, p))
        .collect())
}

/// `POST /my-orders/{id}/cancel` — cancel one of the caller's own pending
/// orders. The filter carries the ownership and state checks, so a foreign
/// or already-processed order matches nothing.
#[post("/my-orders/{id}/cancel")]
pub async fn post_cancel_my_order(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
) -> AppResult {
    let order_id = path.into_inner();

    update!(
        Order,
        &data.db,
        id == order_id && user_id == user.claims.sub && status == OrderStatus::Pending;
        status = OrderStatus::Cancelled
    )
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", "/my-orders"))
        .finish())
}
