//! Orders: a signed-in user places an order against a product; managers view
//! and fulfil orders from the product-manager "Orders" tab. A minimal example
//! of a child resource with its own moderation workflow (`pending` ->
//! `fulfilled`/`cancelled`).
//!
//! The ORM has no joins by design: related rows are fetched with a second
//! query over the collected ids (`Col::in_` on the dynamic builder) and
//! stitched in Rust. For a page-sized list this is one extra indexed query.

use std::collections::{HashMap, HashSet};

use crate::{
    AppData, AppError, AppResult, AppRole, AuthUser, Deserialize, Serialize,
    actix_web::{HttpResponse, delete, get, post, web},
    delete_rows, find, find_one, update,
};

use crate::tables::order::{InsertOrder, Order, OrderStatus};
use crate::tables::product::{Product, ProductStatus};
use crate::tables::user::User;

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

    InsertOrder {
        quantity: form.quantity.max(1),
        note: form.note.clone(),
        ..InsertOrder::new(product.id, user.claims.sub)
    }
    .insert(&data.db)
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

#[derive(Deserialize, Default)]
pub struct OrderSearchParams {
    pub search: Option<String>,
    pub page: Option<i64>,
}

#[derive(Serialize)]
struct ProductOrderRow {
    id: i64,
    quantity: i64,
    note: String,
    status: &'static str,
    created_at: String,
    user_email: String,
    fulfill_url: String,
    delete_url: String,
}

/// `GET /product-manager/{id}/orders` — orders tab: every order placed
/// against this product, filterable by customer email.
#[get("/product-manager/{id}/orders")]
pub async fn get_product_orders(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
    query: web::Query<OrderSearchParams>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.read")?;

    let product_id = path.into_inner();
    let product = Product::fetch(&data.db, product_id)
        .await?
        .ok_or_else(|| AppError::NotFound("product".to_string()))?;

    let search = query.search.as_deref().unwrap_or("").trim().to_lowercase();

    let orders = find!(
        Order,
        &data.db,
        product_id == product_id,
        order_by: created_at.desc()
    )
    .await?;

    // Resolve customer emails, then apply the email search in Rust — one
    // product's orders are a bounded set.
    let user_ids: HashSet<i64> = orders.iter().map(|o| o.user_id).collect();
    let emails: HashMap<i64, String> = User::find()
        .filter(User::ID.in_(user_ids))
        .fetch_all(&data.db)
        .await?
        .into_iter()
        .map(|u| (u.id, u.email))
        .collect();

    let rows: Vec<ProductOrderRow> = orders
        .into_iter()
        .filter_map(|o| {
            let email = emails.get(&o.user_id)?;
            if !search.is_empty() && !email.to_lowercase().contains(&search) {
                return None;
            }
            Some(ProductOrderRow {
                id: o.id,
                quantity: o.quantity,
                note: o.note.unwrap_or_default(),
                status: o.status.as_str(),
                created_at: o.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                user_email: email.clone(),
                fulfill_url: format!("/product-manager/{product_id}/orders/{}", o.id),
                delete_url: format!("/product-manager/{product_id}/orders/{}", o.id),
            })
        })
        .collect();

    Ok(req
        .render_tpl(
            "product-manager/orders",
            &crate::json!({
                "product": { "id": product.id, "name": product.name },
                "rows": rows,
                "search": search,
            }),
        )
        .await)
}

/// `POST /product-manager/{id}/orders/{order_id}` — mark an order fulfilled.
#[post("/product-manager/{id}/orders/{order_id}")]
pub async fn post_fulfill_order(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<(i64, i64)>,
) -> AppResult {
    user.require_permission("products.write")?;
    let (product_id, order_id) = path.into_inner();

    update!(
        Order,
        &data.db,
        id == order_id && product_id == product_id;
        status = OrderStatus::Fulfilled
    )
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", format!("/product-manager/{product_id}/orders")))
        .finish())
}

/// `DELETE /product-manager/{id}/orders/{order_id}` — remove an order.
#[delete("/product-manager/{id}/orders/{order_id}")]
pub async fn delete_order(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<(i64, i64)>,
) -> AppResult {
    user.require_permission("products.write")?;
    let (product_id, order_id) = path.into_inner();

    delete_rows!(Order, &data.db, id == order_id && product_id == product_id).await?;

    Ok(HttpResponse::Ok().finish())
}
