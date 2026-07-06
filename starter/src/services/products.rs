//! Example manageable resource: a product catalog with a public listing +
//! detail page and an admin CRUD UI (`/product-manager/...`). Meant as a
//! template for "the thing your app actually manages" — copy this file,
//! rename `Product`/`products`, and adjust the fields.

use crate::{
    AppData, AppError, AppResult, AppRole, AuthUser, Deserialize, Serialize,
    actix_web::{HttpResponse, delete, get, post, web},
};

const PER_PAGE: i64 = 20;

/// The product lifecycle. `draft` and `archived` are hidden from the public
/// catalog; only `published` products are shown there.
pub const PRODUCT_STATUSES: [&str; 3] = ["draft", "published", "archived"];

fn is_valid_status(status: &str) -> bool {
    PRODUCT_STATUSES.contains(&status)
}

#[derive(Deserialize, Default)]
pub struct ProductSearchParams {
    pub search: Option<String>,
    pub page: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct PublicProductRecord {
    id: i64,
    name: String,
    slug: String,
    description: Option<String>,
    price: f64,
}

/// `GET /products` — public catalog: only `published` products, searchable
/// by name, paginated.
#[get("/products")]
pub async fn get_public_products(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    query: web::Query<ProductSearchParams>,
) -> AppResult {
    use super::RenderTplExt;

    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * PER_PAGE;
    let search = query.search.as_deref().unwrap_or("").trim().to_string();
    let search_pat = format!("%{search}%");

    let sql_where = "status = 'published' AND (? = '' OR name LIKE ?)";

    let total_count: i64 =
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM products WHERE {sql_where}"))
            .bind(&search)
            .bind(&search_pat)
            .fetch_one(&data.db)
            .await?;

    let products = sqlx::query_as::<_, PublicProductRecord>(&format!(
        "SELECT id, name, slug, description, price FROM products WHERE {sql_where} \
         ORDER BY created_at DESC LIMIT ? OFFSET ?"
    ))
    .bind(&search)
    .bind(&search_pat)
    .bind(PER_PAGE)
    .bind(offset)
    .fetch_all(&data.db)
    .await?;

    let total_pages = ((total_count + PER_PAGE - 1) / PER_PAGE).max(1);

    let rows: Vec<crate::serde_json::Value> = products
        .into_iter()
        .map(|p| {
            crate::json!({
                "id": p.id,
                "name": p.name,
                "slug": p.slug,
                "description": p.description.unwrap_or_default(),
                "price": format!("{:.2}", p.price),
            })
        })
        .collect();

    Ok(req
        .render_tpl(
            "products",
            &crate::json!({
                "products": rows,
                "search": search,
                "page": page,
                "total_pages": total_pages,
                "total_count": total_count,
                "per_page": PER_PAGE,
            }),
        )
        .await)
}

#[derive(Deserialize, Default)]
pub struct ProductDetailQuery {
    pub ordered: Option<String>,
}

/// `GET /products/{slug}` — public product detail, with an order form for
/// signed-in users.
#[get("/products/{slug}")]
pub async fn get_public_product_detail(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    path: web::Path<String>,
    query: web::Query<ProductDetailQuery>,
) -> AppResult {
    use super::RenderTplExt;
    let slug = path.into_inner();

    let product = sqlx::query!(
        "SELECT id, name, slug, description, price, status FROM products WHERE slug = ?",
        slug
    )
    .fetch_optional(&data.db)
    .await?;

    let product = match product {
        Some(p) if p.status == "published" => p,
        _ => return Ok(HttpResponse::NotFound().finish()),
    };

    Ok(req
        .render_tpl(
            "products/detail",
            &crate::json!({
                "product": {
                    "id": product.id,
                    "name": product.name,
                    "slug": product.slug,
                    "description": product.description.unwrap_or_default(),
                    "price": format!("{:.2}", product.price),
                },
                "is_logged_in": crate::read_jwt::<AppRole>(&req).is_ok(),
                "ordered": query.ordered.as_deref() == Some("1"),
            }),
        )
        .await)
}

#[derive(sqlx::FromRow)]
struct ProductManagerRecord {
    id: i64,
    name: String,
    price: f64,
    status: String,
    created_at: String,
}

#[derive(Serialize)]
struct ProductManagerRow {
    id: i64,
    name: String,
    price: String,
    status: String,
    created_at: String,
    link: String,
    delete_url: String,
}

/// `GET /product-manager` — admin catalog table (search + pagination).
#[get("/product-manager")]
pub async fn get_products(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    query: web::Query<ProductSearchParams>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.read")?;

    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * PER_PAGE;
    let search = query.search.as_deref().unwrap_or("").trim().to_string();
    let search_pat = format!("%{search}%");

    let sql_where = "(? = '' OR name LIKE ?)";

    let total_count: i64 =
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM products WHERE {sql_where}"))
            .bind(&search)
            .bind(&search_pat)
            .fetch_one(&data.db)
            .await?;

    let records = sqlx::query_as::<_, ProductManagerRecord>(&format!(
        "SELECT id, name, price, status, created_at FROM products WHERE {sql_where} \
         ORDER BY created_at DESC LIMIT ? OFFSET ?"
    ))
    .bind(&search)
    .bind(&search_pat)
    .bind(PER_PAGE)
    .bind(offset)
    .fetch_all(&data.db)
    .await?;

    let rows: Vec<ProductManagerRow> = records
        .into_iter()
        .map(|p| ProductManagerRow {
            id: p.id,
            name: p.name,
            price: format!("{:.2}", p.price),
            status: p.status,
            created_at: p.created_at,
            link: format!("/product-manager/{}", p.id),
            delete_url: format!("/product-manager/{}", p.id),
        })
        .collect();

    let total_pages = ((total_count + PER_PAGE - 1) / PER_PAGE).max(1);

    Ok(req
        .render_tpl(
            "product-manager",
            &crate::json!({
                "rows": rows,
                "search": search,
                "page": page,
                "total_pages": total_pages,
                "total_count": total_count,
                "per_page": PER_PAGE,
            }),
        )
        .await)
}

#[get("/product-manager/create")]
pub async fn get_product_create(req: actix_web::HttpRequest, user: AuthUser<AppRole>) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.write")?;

    Ok(req
        .render_tpl(
            "product-manager/create",
            &crate::json!({
                "name": "",
                "slug": "",
                "description": "",
                "price": "",
                "error_slug": "",
            }),
        )
        .await)
}

#[derive(Deserialize, Serialize)]
pub struct ProductForm {
    pub name: String,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub price: f64,
    pub status: Option<String>,
}

fn generate_slug(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join("-")
}

#[post("/product-manager/create")]
pub async fn post_product_create(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    form: web::Form<ProductForm>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.write")?;

    let slug = form
        .slug
        .clone()
        .unwrap_or_else(|| generate_slug(&form.name));

    let existing = sqlx::query!("SELECT id FROM products WHERE slug = ?", slug)
        .fetch_optional(&data.db)
        .await?;

    if existing.is_some() {
        return Ok(req
            .render_tpl(
                "product-manager/create",
                &crate::json!({
                    "name": form.name,
                    "slug": slug,
                    "description": form.description,
                    "price": format!("{:.2}", form.price),
                    "error_slug": "slug_taken",
                }),
            )
            .await);
    }

    sqlx::query!(
        "INSERT INTO products (name, slug, description, price) VALUES (?, ?, ?, ?)",
        form.name,
        slug,
        form.description,
        form.price
    )
    .execute(&data.db)
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", "/product-manager"))
        .finish())
}

#[get("/product-manager/{id}")]
pub async fn get_product(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.read")?;

    let product_id = path.into_inner();
    let product = sqlx::query!(
        "SELECT id, name, slug, description, price, status FROM products WHERE id = ?",
        product_id
    )
    .fetch_optional(&data.db)
    .await?
    .ok_or_else(|| AppError::NotFound("product".to_string()))?;

    Ok(req
        .render_tpl(
            "product-manager/details",
            &crate::json!({
                "product": {
                    "id": product.id,
                    "name": product.name,
                    "slug": product.slug,
                    "description": product.description.unwrap_or_default(),
                    "price": format!("{:.2}", product.price),
                    "status": product.status,
                },
                "error_slug": "",
            }),
        )
        .await)
}

#[post("/product-manager/{id}")]
pub async fn post_product(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
    form: web::Form<ProductForm>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("products.write")?;

    let product_id = path.into_inner();
    let slug = form
        .slug
        .clone()
        .unwrap_or_else(|| generate_slug(&form.name));

    let current_status: String = sqlx::query_scalar("SELECT status FROM products WHERE id = ?")
        .bind(product_id)
        .fetch_optional(&data.db)
        .await?
        .unwrap_or_else(|| "draft".to_string());

    let status = match form.status.as_deref() {
        Some(s) if is_valid_status(s) => s.to_string(),
        _ => current_status,
    };

    let existing = sqlx::query!(
        "SELECT id FROM products WHERE slug = ? AND id != ?",
        slug,
        product_id
    )
    .fetch_optional(&data.db)
    .await?;

    if existing.is_some() {
        return Ok(req
            .render_tpl(
                "product-manager/details",
                &crate::json!({
                    "product": {
                        "id": product_id,
                        "name": form.name,
                        "slug": slug,
                        "description": form.description,
                        "price": format!("{:.2}", form.price),
                        "status": status,
                    },
                    "error_slug": "slug_taken",
                }),
            )
            .await);
    }

    sqlx::query!(
        "UPDATE products SET name = ?, slug = ?, description = ?, price = ?, status = ? WHERE id = ?",
        form.name,
        slug,
        form.description,
        form.price,
        status,
        product_id
    )
    .execute(&data.db)
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", format!("/product-manager/{product_id}")))
        .finish())
}

#[delete("/product-manager/{id}")]
pub async fn delete_product(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
) -> AppResult {
    user.require_permission("products.write")?;

    let product_id = path.into_inner();
    sqlx::query!("DELETE FROM products WHERE id = ?", product_id)
        .execute(&data.db)
        .await?;

    Ok(HttpResponse::Ok().finish())
}
