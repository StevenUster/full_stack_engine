//! Orders: a signed-in user places an order against a product; managers view
//! and fulfil orders from the product-manager "Orders" tab. A minimal example
//! of a child resource with its own moderation workflow (`pending` ->
//! `fulfilled`/`cancelled`).

use crate::{
    AppData, AppError, AppResult, AppRole, AuthUser, Deserialize, Serialize,
    actix_web::{HttpResponse, delete, get, post, web},
};

pub const ORDER_STATUSES: [&str; 3] = ["pending", "fulfilled", "cancelled"];

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

    let product = sqlx::query!(
        "SELECT id FROM products WHERE slug = ? AND status = 'published'",
        slug
    )
    .fetch_optional(&data.db)
    .await?
    .ok_or_else(|| AppError::NotFound("product".to_string()))?;

    let quantity = form.quantity.max(1);

    sqlx::query!(
        "INSERT INTO orders (product_id, user_id, quantity, note) VALUES (?, ?, ?, ?)",
        product.id,
        user.claims.sub,
        quantity,
        form.note
    )
    .execute(&data.db)
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", format!("/products/{slug}?ordered=1")))
        .finish())
}

#[derive(sqlx::FromRow)]
struct MyOrderRecord {
    id: i64,
    quantity: i64,
    note: Option<String>,
    status: String,
    created_at: String,
    product_name: String,
    product_slug: String,
}

#[derive(Serialize)]
struct MyOrderRow {
    id: i64,
    quantity: i64,
    note: String,
    status: String,
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

    let records = sqlx::query_as::<_, MyOrderRecord>(
        "SELECT o.id, o.quantity, o.note, o.status, o.created_at, \
                p.name AS product_name, p.slug AS product_slug \
         FROM orders o JOIN products p ON o.product_id = p.id \
         WHERE o.user_id = ? ORDER BY o.created_at DESC",
    )
    .bind(user.claims.sub)
    .fetch_all(&data.db)
    .await?;

    let rows: Vec<MyOrderRow> = records
        .into_iter()
        .map(|o| MyOrderRow {
            id: o.id,
            quantity: o.quantity,
            note: o.note.unwrap_or_default(),
            status: o.status,
            created_at: o.created_at,
            product_name: o.product_name,
            product_link: format!("/products/{}", o.product_slug),
        })
        .collect();

    Ok(req.render_tpl("my-orders", &crate::json!({ "rows": rows })).await)
}

/// `POST /my-orders/{id}/cancel` — cancel one of the caller's own pending
/// orders.
#[post("/my-orders/{id}/cancel")]
pub async fn post_cancel_my_order(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
) -> AppResult {
    let order_id = path.into_inner();

    sqlx::query!(
        "UPDATE orders SET status = 'cancelled' \
         WHERE id = ? AND user_id = ? AND status = 'pending'",
        order_id,
        user.claims.sub
    )
    .execute(&data.db)
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

#[derive(sqlx::FromRow)]
struct ProductOrderRecord {
    id: i64,
    quantity: i64,
    note: Option<String>,
    status: String,
    created_at: String,
    user_email: String,
}

#[derive(Serialize)]
struct ProductOrderRow {
    id: i64,
    quantity: i64,
    note: String,
    status: String,
    created_at: String,
    user_email: String,
    fulfill_url: String,
    delete_url: String,
}

/// `GET /product-manager/{id}/orders` — orders tab: every order placed
/// against this product.
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
    let product = sqlx::query!("SELECT id, name FROM products WHERE id = ?", product_id)
        .fetch_optional(&data.db)
        .await?
        .ok_or_else(|| AppError::NotFound("product".to_string()))?;

    let search = query.search.as_deref().unwrap_or("").trim().to_string();
    let pattern = format!("%{search}%");

    let records = sqlx::query_as::<_, ProductOrderRecord>(
        "SELECT o.id, o.quantity, o.note, o.status, o.created_at, u.email AS user_email \
         FROM orders o JOIN users u ON o.user_id = u.id \
         WHERE o.product_id = ? AND (? = '' OR u.email LIKE ?) \
         ORDER BY o.created_at DESC",
    )
    .bind(product_id)
    .bind(&search)
    .bind(&pattern)
    .fetch_all(&data.db)
    .await?;

    let rows: Vec<ProductOrderRow> = records
        .into_iter()
        .map(|o| ProductOrderRow {
            id: o.id,
            quantity: o.quantity,
            note: o.note.unwrap_or_default(),
            status: o.status,
            created_at: o.created_at,
            user_email: o.user_email,
            fulfill_url: format!("/product-manager/{product_id}/orders/{}", o.id),
            delete_url: format!("/product-manager/{product_id}/orders/{}", o.id),
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

    sqlx::query!(
        "UPDATE orders SET status = 'fulfilled' WHERE id = ? AND product_id = ?",
        order_id,
        product_id
    )
    .execute(&data.db)
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

    sqlx::query!(
        "DELETE FROM orders WHERE id = ? AND product_id = ?",
        order_id,
        product_id
    )
    .execute(&data.db)
    .await?;

    Ok(HttpResponse::Ok().finish())
}
