//! Public, unauthenticated JSON API for external sites.
//!
//! Only data already shown on the public catalog is exposed (`published`
//! products). Two framework pieces do the rest:
//!
//! * **Cross-origin access** is middleware, configured by
//!   `CORS_ALLOWED_ORIGINS` (or `.cors(...)` in `lib.rs`) — not a header
//!   pasted onto each response, which is how the `Vary: Origin` and the
//!   preflight handler used to go missing.
//! * **The spec and the docs page** are mounted by `.api_docs(...)`. The
//!   `#[model(api)]` half is generated from the model registry; the two
//!   hand-written endpoints below declare themselves in [`openapi_paths`],
//!   because generation cannot know them.

use crate::{
    AppData, AppResult, Deserialize,
    actix_web::{HttpResponse, get, web},
};

use crate::models::product::{Product, ProductStatus};

const PER_PAGE: i64 = 50;

/// The hand-written routes of this file, in `OpenAPI` `paths` shape.
///
/// `/api/products` is an *override* of the generated endpoint — published rows
/// only, with `search`/`page` — so its real contract differs from the
/// generated one and has to be spelled out. Merged over the generated document
/// by `.api_docs(...)`, so turning `api` on for another model later adds its
/// paths without touching this.
pub fn openapi_paths() -> crate::serde_json::Value {
    crate::json!({
        "/api/products": {
            "get": {
                "summary": "List published products",
                "operationId": "list_published_products",
                "security": [],
                "parameters": [
                    { "name": "search", "in": "query", "schema": { "type": "string" } },
                    { "name": "page", "in": "query", "schema": { "type": "integer", "minimum": 1, "default": 1 } }
                ],
                "responses": {
                    "200": {
                        "description": "Paginated list of products",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/ProductList" } } }
                    }
                }
            }
        },
        "/api/products/{slug}": {
            "get": {
                "summary": "Get a single published product",
                "operationId": "get_published_product",
                "security": [],
                "parameters": [
                    { "name": "slug", "in": "path", "required": true, "schema": { "type": "string" } }
                ],
                "responses": {
                    "200": {
                        "description": "Product detail",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/PublicProduct" } } }
                    },
                    "404": {
                        "description": "Product not found or not published",
                        "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } }
                    }
                }
            }
        }
    })
}

/// What those two endpoints actually return — narrower than the `Product` row
/// (no `status`, no timestamps). A public API exposing the whole row would be
/// the bug this override exists to avoid.
pub fn openapi_schemas() -> crate::serde_json::Value {
    crate::json!({
        "PublicProduct": {
            "type": "object",
            "required": ["id", "name", "slug", "price", "url"],
            "properties": {
                "id": { "type": "integer", "format": "int64" },
                "name": { "type": "string" },
                "slug": { "type": "string" },
                "description": { "type": "string", "nullable": true },
                "price": { "type": "string" },
                "url": { "type": "string", "description": "Relative path to the public product page" }
            }
        },
        "ProductList": {
            "type": "object",
            "required": ["products", "page", "per_page", "total_pages", "total_count"],
            "properties": {
                "products": { "type": "array", "items": { "$ref": "#/components/schemas/PublicProduct" } },
                "page": { "type": "integer" },
                "per_page": { "type": "integer" },
                "total_pages": { "type": "integer" },
                "total_count": { "type": "integer" }
            }
        }
    })
}

#[derive(Deserialize, Default)]
pub struct ApiProductsQuery {
    pub search: Option<String>,
    pub page: Option<i64>,
}

/// `GET /api/products` — paginated list of published products.
#[get("/api/products")]
pub async fn get_products(
    data: web::Data<AppData>,
    query: web::Query<ApiProductsQuery>,
) -> AppResult {
    let page = query.page.unwrap_or(1).max(1);
    let search = query.search.as_deref().unwrap_or("").trim().to_string();

    let result = crate::find_page!(
        Product,
        &data.db,
        status == ProductStatus::Published && name.contains_opt(&search),
        order_by: created_at.desc(),
        page: page,
        per_page: PER_PAGE
    )
    .await?;

    let total_pages = ((result.total + PER_PAGE - 1) / PER_PAGE).max(1);

    let rows: Vec<crate::serde_json::Value> = result
        .rows
        .into_iter()
        .map(|p| {
            crate::json!({
                "id": p.id,
                "name": p.name,
                "slug": p.slug,
                "description": p.description.unwrap_or_default(),
                "price": format!("{:.2}", p.price),
                "url": format!("/products/{}", p.slug),
            })
        })
        .collect();

    Ok(HttpResponse::Ok().json(crate::json!({
        "products": rows,
        "page": page,
        "per_page": PER_PAGE,
        "total_pages": total_pages,
        "total_count": result.total,
    })))
}

/// `GET /api/products/{slug}` — a single published product.
#[get("/api/products/{slug}")]
pub async fn get_product_detail(data: web::Data<AppData>, path: web::Path<String>) -> AppResult {
    let slug = path.into_inner();

    let product = crate::find_one!(Product, &data.db, slug == slug.as_str()).await?;

    let product = match product {
        Some(p) if p.status == ProductStatus::Published => p,
        _ => return Ok(HttpResponse::NotFound().finish()),
    };

    Ok(HttpResponse::Ok().json(crate::json!({
        "id": product.id,
        "name": product.name,
        "slug": product.slug,
        "description": product.description.unwrap_or_default(),
        "price": format!("{:.2}", product.price),
        "url": format!("/products/{}", product.slug),
    })))
}
