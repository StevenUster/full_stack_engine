//! Public, unauthenticated JSON API for external sites — a self-hosted
//! Swagger UI at `/api/docs` reading a spec from `/api/openapi.json`. Only
//! data already shown on the public catalog is exposed here (`published`
//! products); everything is CORS-enabled so it can be consumed cross-origin.

use crate::{
    AppData, AppResult, Deserialize,
    actix_web::{HttpResponse, get, web},
};

use super::RenderTplExt;
use crate::models::product::{Product, ProductStatus};

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
///
/// Built in two halves, which is the pattern to copy:
///
/// * `models::openapi::spec` describes every `#[model(api)]` endpoint straight
///   from the model registry — add `api` to a struct and it documents itself,
///   with the right column names, types and nullability, and no chance of
///   drifting from what the endpoint actually returns.
/// * the hand-written routes below are declared explicitly, because generation
///   cannot know them. `/api/products` here is an *override* (published rows
///   only, `search`/`page` parameters), so its real contract differs from the
///   generated one and has to be spelled out.
#[get("/api/openapi.json")]
pub async fn get_openapi_spec(data: web::Data<AppData>) -> AppResult {
    use full_stack_engine::models::openapi;

    let base_url = data.config.base_url();
    let mut spec = openapi::spec(&openapi::Info {
        title: "Starter Public API",
        version: env!("CARGO_PKG_VERSION"),
        description: "Read-only, unauthenticated access to the published product catalog.",
        base_url: &base_url,
    });

    // The two hand-written endpoints. Merged in rather than replacing the
    // document, so flipping `api` on a model later adds its paths here without
    // touching this function.
    let hand_written = crate::json!({
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
    });

    // The shapes those two hand-written endpoints actually return, which are
    // narrower than the `Product` row (no `status`, no timestamps) — a public
    // API exposing the whole row would be the bug this override exists to avoid.
    let hand_written_schemas = crate::json!({
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
    });

    merge_objects(&mut spec["paths"], &hand_written);
    merge_objects(&mut spec["components"]["schemas"], &hand_written_schemas);

    Ok(json_ok(spec))
}

/// Copies `extra`'s keys into `target`, both of which are expected to be JSON
/// objects. A shallow merge is all that's needed: the two halves of the spec
/// contribute disjoint path and schema names.
fn merge_objects(target: &mut crate::serde_json::Value, extra: &crate::serde_json::Value) {
    if let (Some(target), Some(extra)) = (target.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
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

    Ok(json_ok(crate::json!({
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

    Ok(json_ok(crate::json!({
        "id": product.id,
        "name": product.name,
        "slug": product.slug,
        "description": product.description.unwrap_or_default(),
        "price": format!("{:.2}", product.price),
        "url": format!("/products/{}", product.slug),
    })))
}
