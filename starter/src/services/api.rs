//! Example public, unauthenticated JSON API scaffolding — a self-hosted
//! Swagger UI at `/api/docs` reading a spec from `/api/openapi.json`. Real
//! apps should replace `paths`/`components.schemas` below with their own
//! public endpoints; the `cors_json` helper and docs page are meant to be
//! reused as-is.

use crate::{
    AppData, AppResult,
    actix_web::{HttpResponse, get, web},
};

use super::RenderTplExt;

/// Attaches permissive CORS headers so any external origin can read the
/// response. Reuse this for every public API endpoint.
pub fn cors_json(body: crate::serde_json::Value) -> HttpResponse {
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

    Ok(cors_json(crate::json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Starter Public API",
            "version": "1.0.0",
            "description": "Read-only, unauthenticated public API. Replace this example spec with your own paths/schemas."
        },
        "servers": [ { "url": base_url } ],
        "paths": {},
        "components": { "schemas": {} }
    })))
}
