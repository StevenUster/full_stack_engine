//! Public, unauthenticated JSON API for external sites — a self-hosted
//! Swagger UI at `/api/docs` reading a spec from `/api/openapi.json`. Only
//! data already shown on the public catalog is exposed here (`published`
//! products); everything is CORS-enabled so it can be consumed cross-origin.

use crate::{
    AppData, AppResult, Deserialize,
    actix_web::{HttpResponse, get, web},
};

use super::RenderTplExt;

const PER_PAGE: i64 = 50;

/// Attach permissive CORS headers so any external origin can read the data.
fn json_ok(body: crate::serde_json::Value) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header(("Access-Control-Allow-Origin", "*"))
        .insert_header(("Access-Control-Allow-Methods", "GET, OPTIONS"))
        .json(body)
}

/// `GET /api/docs` — self-hosted Swagger UI rendering the spec below.
#[get("/api/docs")]
pub async fn get_docs(req: actix_web::HttpRequest) -> AppResult {
    Ok(req.render_tpl("api/docs", &crate::json!({})).await)
}

/// `GET /api/openapi.json` — machine-readable `OpenAPI` 3.0 spec describing
/// this API, so external consumers can import it into Swagger UI / Postman or
/// generate a client.
#[get("/api/openapi.json")]
pub async fn get_openapi_spec(data: web::Data<AppData>) -> AppResult {
    let base_url = format!("{}://{}", data.protocol, data.domain);

    Ok(json_ok(crate::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Starter Public API",
            "version": "1.0.0",
            "description": "Read-only, unauthenticated access to the published product catalog."
        },
        "servers": [ { "url": base_url } ],
        "paths": {
            "/api/products": {
                "get": {
                    "summary": "List published products",
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
                    "parameters": [
                        { "name": "slug", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "responses": {
                        "200": {
                            "description": "Product detail",
                            "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Product" } } }
                        },
                        "404": { "description": "Product not found or not published" }
                    }
                }
            }
        },
        "components": {
            "schemas": {
                "Product": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "integer" },
                        "name": { "type": "string" },
                        "slug": { "type": "string" },
                        "description": { "type": "string" },
                        "price": { "type": "string" },
                        "url": { "type": "string", "description": "Relative path to the public product page" }
                    }
                },
                "ProductList": {
                    "type": "object",
                    "properties": {
                        "products": { "type": "array", "items": { "$ref": "#/components/schemas/Product" } },
                        "page": { "type": "integer" },
                        "per_page": { "type": "integer" },
                        "total_pages": { "type": "integer" },
                        "total_count": { "type": "integer" }
                    }
                }
            }
        }
    })))
}

#[derive(Deserialize, Default)]
pub struct ApiProductsQuery {
    pub search: Option<String>,
    pub page: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct ApiProductRecord {
    id: i64,
    name: String,
    slug: String,
    description: Option<String>,
    price: f64,
}

/// `GET /api/products` — paginated list of published products.
#[get("/api/products")]
pub async fn get_products(
    data: web::Data<AppData>,
    query: web::Query<ApiProductsQuery>,
) -> AppResult {
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

    let products = sqlx::query_as::<_, ApiProductRecord>(&format!(
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
                "url": format!("/products/{}", p.slug),
            })
        })
        .collect();

    Ok(json_ok(crate::json!({
        "products": rows,
        "page": page,
        "per_page": PER_PAGE,
        "total_pages": total_pages,
        "total_count": total_count,
    })))
}

/// `GET /api/products/{slug}` — a single published product.
#[get("/api/products/{slug}")]
pub async fn get_product_detail(data: web::Data<AppData>, path: web::Path<String>) -> AppResult {
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

    Ok(json_ok(crate::json!({
        "id": product.id,
        "name": product.name,
        "slug": product.slug,
        "description": product.description.unwrap_or_default(),
        "price": format!("{:.2}", product.price),
        "url": format!("/products/{}", product.slug),
    })))
}
