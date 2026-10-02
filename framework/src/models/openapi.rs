//! An `OpenAPI` 3.0 document generated from the model registry.
//!
//! The generated `/api/...` routes already exist because a `#[model]` struct
//! declared `api`, and the registry already knows every column's name, SQL type
//! and nullability. Writing the spec by hand therefore duplicates information
//! the binary has — and duplicated information drifts: a column added to a
//! struct appears in the API immediately and in a hand-written `json!` blob
//! whenever someone remembers.
//!
//! This is the same idea as the rest of the framework: the struct is the
//! source of truth, and the surface around it is derived.
//!
//! ```ignore
//! #[get("/api/openapi.json")]
//! async fn openapi() -> AppResult {
//!     Ok(HttpResponse::Ok().json(models::openapi::spec(&openapi::Info {
//!         title: "Example API",
//!         version: env!("CARGO_PKG_VERSION"),
//!         description: "Public read-only data.",
//!         base_url: &data.config.base_url(),
//!     })))
//! }
//! ```
//!
//! Hand-written routes are described by handing their paths to
//! [`crate::FrameworkApp::api_docs`], which merges them into this document and
//! mounts both `/api/openapi.json` and a browsable `/api/docs`. Before that
//! existed, an app with a public API kept a second, hand-maintained `json!`
//! spec beside the generated one — and the hand-written half drifts, because
//! nothing fails when it does.

use actix_web::{HttpResponse, web};
use serde_json::{Value, json};

use fse_schema::model::SqlType;

use super::{ModelMeta, registered_models};

/// The document's identity — everything that is about the app rather than about
/// its models.
pub struct Info<'a> {
    pub title: &'a str,
    pub version: &'a str,
    pub description: &'a str,
    /// Public base URL, e.g. `https://example.com` (see
    /// [`crate::config::Config::base_url`]).
    pub base_url: &'a str,
}

/// Builds the document for every registered model that declared `api` and is
/// not `disabled`.
///
/// Authentication is described per path: a model with `public_read` is reachable
/// anonymously, anything else needs the session cookie and the model's
/// `<base>.read` permission.
#[must_use]
pub fn spec(info: &Info<'_>) -> Value {
    let mut paths = serde_json::Map::new();
    let mut schemas = serde_json::Map::new();

    for meta in registered_models() {
        if meta.ui.disabled || !meta.ui.api {
            continue;
        }
        let table = &meta.table.name;
        let schema_name = &meta.table.struct_name;
        schemas.insert(schema_name.clone(), row_schema(meta));

        let public = meta.ui.public_read.is_some();
        paths.insert(format!("/api/{table}"), list_path(meta, public));
        // A `public_read` model is looked up by that column; everything else by
        // primary key. This mirrors how the routes are actually mounted.
        let (param_name, param_schema, param_desc) = if let Some(column) = meta.ui.public_read {
            (
                "key",
                json!({ "type": "string" }),
                format!("The row's unique `{column}`."),
            )
        } else {
            (
                "id",
                json!({ "type": "integer", "format": "int64" }),
                "The row's primary key.".to_string(),
            )
        };
        paths.insert(
            format!("/api/{table}/{{{param_name}}}"),
            detail_path(meta, public, param_name, &param_schema, &param_desc),
        );
    }

    schemas.insert("Error".to_string(), error_schema());

    json!({
        "openapi": "3.0.3",
        "info": {
            "title": info.title,
            "version": info.version,
            "description": info.description,
        },
        "servers": [{ "url": info.base_url }],
        "paths": Value::Object(paths),
        "components": {
            "schemas": Value::Object(schemas),
            "securitySchemes": {
                // The framework authenticates with a JWT in an HttpOnly cookie,
                // so this is `apiKey`/`cookie` rather than `http`/`bearer` —
                // describing it as a bearer token would tell a client to do
                // something that does not work.
                "sessionCookie": {
                    "type": "apiKey",
                    "in": "cookie",
                    "name": "token",
                },
            },
        },
    })
}

/// `GET /api/{table}` — the paginated list endpoint.
fn list_path(meta: &ModelMeta, public: bool) -> Value {
    let table = &meta.table.name;
    let mut operation = json!({
        "summary": format!("List {table}"),
        "operationId": format!("list_{table}"),
        "parameters": [
            { "name": "page", "in": "query", "required": false,
              "schema": { "type": "integer", "minimum": 1, "default": 1 },
              "description": "1-based page number." },
            { "name": "per_page", "in": "query", "required": false,
              "schema": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
              "description": "Rows per page; clamped to 100." },
            { "name": "q", "in": "query", "required": false,
              "schema": { "type": "string" },
              "description": "Free-text search across the model's searchable columns." },
        ],
        "responses": {
            "200": {
                "description": "A page of rows.",
                "content": { "application/json": { "schema": {
                    "type": "object",
                    "required": ["rows", "total", "page", "per_page", "total_pages"],
                    "properties": {
                        "rows": { "type": "array", "items": schema_ref(meta) },
                        "total": { "type": "integer", "description": "Rows matching the query, across all pages." },
                        "page": { "type": "integer" },
                        "per_page": { "type": "integer" },
                        "total_pages": { "type": "integer" },
                    },
                } } },
            },
        },
    });
    apply_auth(&mut operation, public);
    json!({ "get": operation })
}

/// `GET /api/{table}/{id|key}` — one row.
fn detail_path(
    meta: &ModelMeta,
    public: bool,
    param_name: &str,
    param_schema: &Value,
    param_desc: &str,
) -> Value {
    let table = &meta.table.name;
    let mut operation = json!({
        "summary": format!("Fetch one row from {table}"),
        "operationId": format!("get_{table}"),
        "parameters": [{
            "name": param_name,
            "in": "path",
            "required": true,
            "schema": param_schema,
            "description": param_desc,
        }],
        "responses": {
            "200": {
                "description": "The row.",
                "content": { "application/json": { "schema": schema_ref(meta) } },
            },
            "404": {
                "description": "No such row.",
                "content": { "application/json": { "schema": {
                    "$ref": "#/components/schemas/Error"
                } } },
            },
        },
    });
    apply_auth(&mut operation, public);
    json!({ "get": operation })
}

/// Adds the security requirement and the 401 response to a non-public
/// operation. A public one gets an explicit empty `security`, which is how
/// `OpenAPI` says "no authentication needed" rather than leaving it ambiguous.
fn apply_auth(operation: &mut Value, public: bool) {
    let Some(obj) = operation.as_object_mut() else {
        return;
    };
    if public {
        obj.insert("security".to_string(), json!([]));
        return;
    }
    obj.insert("security".to_string(), json!([{ "sessionCookie": [] }]));
    if let Some(responses) = obj.get_mut("responses").and_then(Value::as_object_mut) {
        responses.insert(
            "401".to_string(),
            json!({
                "description": "Missing, expired or insufficiently privileged session.",
                "content": { "application/json": { "schema": {
                    "$ref": "#/components/schemas/Error"
                } } },
            }),
        );
    }
}

fn schema_ref(meta: &ModelMeta) -> Value {
    json!({ "$ref": format!("#/components/schemas/{}", meta.table.struct_name) })
}

/// One model's row schema, from its columns.
///
/// Hidden columns (`#[ui(hidden)]`, json/blob, secret-looking names) are
/// left out, exactly as the generated rows leave them out — the schema
/// describes what the API returns.
fn row_schema(meta: &ModelMeta) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for column in &meta.table.columns {
        // Private columns never reach the public API; an authenticated API
        // documents them too, but one schema serves both, so the public
        // shape is the documented one.
        let hidden = meta
            .ui_field(&column.name)
            .is_some_and(|f| f.hidden || (f.private && meta.ui.public_read.is_some()));
        if hidden && !column.primary_key {
            continue;
        }
        properties.insert(column.name.clone(), column_schema(column));
        if !column.nullable {
            required.push(Value::String(column.name.clone()));
        }
    }

    json!({
        "type": "object",
        "title": meta.table.struct_name,
        "required": required,
        "properties": Value::Object(properties),
    })
}

fn column_schema(column: &fse_schema::model::ColumnDef) -> Value {
    // A JSON column holds arbitrary serialised data, and an enum/text column a
    // constrained string — neither is described by its SQL storage type.
    let mut schema = if column.json {
        json!({ "description": "Arbitrary JSON, stored as text." })
    } else if column.is_enum {
        json!({ "type": "string" })
    } else {
        match column.ty {
            SqlType::Integer => json!({ "type": "integer", "format": "int64" }),
            SqlType::Real => json!({ "type": "number", "format": "double" }),
            SqlType::Boolean => json!({ "type": "boolean" }),
            SqlType::Timestamp => json!({ "type": "string", "format": "date-time" }),
            SqlType::Text => json!({ "type": "string" }),
            SqlType::Blob => json!({ "type": "string", "format": "byte" }),
        }
    };

    if let Some(obj) = schema.as_object_mut() {
        // `OpenAPI` 3.0 has no union types, so a nullable field is marked with
        // the 3.0 `nullable` keyword rather than `type: [x, "null"]`.
        if column.nullable {
            obj.insert("nullable".to_string(), json!(true));
        }
        if column.primary_key {
            obj.insert("readOnly".to_string(), json!(true));
        }
    }
    schema
}

/// The shape every framework error response has: see
/// [`crate::error::AppError`], whose body carries the user-safe message.
fn error_schema() -> Value {
    json!({
        "type": "object",
        "title": "Error",
        "properties": {
            "error": {
                "type": "string",
                "description": "A message safe to show a user. Internal details are never included.",
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document is valid enough for a generator to consume: the pieces a
    /// client toolchain requires are present and internally consistent.
    #[test]
    fn spec_is_well_formed_even_with_no_api_models() {
        let doc = spec(&Info {
            title: "Test API",
            version: "1.2.3",
            description: "d",
            base_url: "https://example.com",
        });
        assert_eq!(doc["openapi"], "3.0.3");
        assert_eq!(doc["info"]["title"], "Test API");
        assert_eq!(doc["info"]["version"], "1.2.3");
        assert_eq!(doc["servers"][0]["url"], "https://example.com");
        // The error schema and the security scheme are always defined, so a
        // `$ref` to either can never dangle.
        assert!(doc["components"]["schemas"]["Error"].is_object());
        assert_eq!(
            doc["components"]["securitySchemes"]["sessionCookie"]["in"],
            "cookie"
        );
        assert!(doc["paths"].is_object());
    }

    #[test]
    fn nullable_and_primary_key_columns_are_described() {
        use fse_schema::model::ColumnDef;

        let column = |name: &str, ty, nullable, primary_key| ColumnDef {
            name: name.to_string(),
            rust_type: "X".to_string(),
            ty,
            nullable,
            primary_key,
            unique: false,
            json: false,
            is_enum: false,
            index: false,
            default: None,
            references: None,
            check_in: None,
            renamed_from: None,
        };

        let id = column_schema(&column("id", SqlType::Integer, false, true));
        assert_eq!(id["type"], "integer");
        // A client must not be told to send the primary key on a write.
        assert_eq!(id["readOnly"], true);
        assert!(id.get("nullable").is_none());

        let note = column_schema(&column("note", SqlType::Text, true, false));
        assert_eq!(note["type"], "string");
        assert_eq!(note["nullable"], true);

        let created = column_schema(&column("created_at", SqlType::Timestamp, false, false));
        assert_eq!(created["format"], "date-time");

        let published = column_schema(&column("published", SqlType::Boolean, false, false));
        assert_eq!(published["type"], "boolean");
    }

    #[test]
    fn public_and_private_operations_declare_different_security() {
        let mut public = json!({ "responses": {} });
        apply_auth(&mut public, true);
        // An explicit empty list is how `OpenAPI` says "no auth", as opposed to
        // omitting the key, which means "inherit whatever is global".
        assert_eq!(public["security"], json!([]));
        assert!(public["responses"].get("401").is_none());

        let mut private = json!({ "responses": {} });
        apply_auth(&mut private, false);
        assert_eq!(private["security"], json!([{ "sessionCookie": [] }]));
        assert!(
            private["responses"]["401"].is_object(),
            "an authenticated endpoint has to document its 401"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Serving the document
// ─────────────────────────────────────────────────────────────────────────────

/// What [`crate::FrameworkApp::api_docs`] needs to describe an app's API.
///
/// The model-derived half is generated; `paths` and `schemas` are where an app
/// describes the routes it wrote by hand.
#[derive(Clone)]
pub struct ApiDocs {
    pub title: String,
    pub version: String,
    pub description: String,
    /// Hand-written paths, in `OpenAPI` `paths` shape:
    /// `json!({ "/api/events": { "get": { ... } } })`. Merged over the
    /// generated ones, so an entry here replaces a generated path of the same
    /// name.
    pub paths: Value,
    /// Hand-written `components.schemas` entries, merged the same way.
    pub schemas: Value,
}

impl ApiDocs {
    /// A document with no hand-written routes — everything from the model
    /// registry.
    #[must_use]
    pub fn new(
        title: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            version: version.into(),
            description: description.into(),
            paths: json!({}),
            schemas: json!({}),
        }
    }

    /// Adds hand-written paths, in `OpenAPI` `paths` shape.
    #[must_use]
    pub fn paths(mut self, paths: Value) -> Self {
        self.paths = paths;
        self
    }

    /// Adds hand-written `components.schemas` entries.
    #[must_use]
    pub fn schemas(mut self, schemas: Value) -> Self {
        self.schemas = schemas;
        self
    }

    /// The finished document for `base_url`.
    #[must_use]
    pub fn build(&self, base_url: &str) -> Value {
        let mut doc = spec(&Info {
            title: &self.title,
            version: &self.version,
            description: &self.description,
            base_url,
        });
        merge_object(&mut doc["paths"], &self.paths);
        merge_object(&mut doc["components"]["schemas"], &self.schemas);
        doc
    }
}

/// Shallow merge: `overlay`'s keys replace `target`'s.
///
/// Shallow on purpose. A path item is described as a whole — merging two
/// halves of one operation would produce a document that is neither what the
/// generator emitted nor what the app wrote.
fn merge_object(target: &mut Value, overlay: &Value) {
    let (Some(target), Some(overlay)) = (target.as_object_mut(), overlay.as_object()) else {
        return;
    };
    for (key, value) in overlay {
        target.insert(key.clone(), value.clone());
    }
}

/// The HTML of the `/api/docs` page: Swagger UI pointed at
/// `/api/openapi.json`.
fn docs_page(title: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<link rel="stylesheet" href="{SWAGGER_CDN}/swagger-ui.css">
</head>
<body>
<div id="swagger"></div>
<script src="{SWAGGER_CDN}/swagger-ui-bundle.js" crossorigin></script>
<script>
  window.addEventListener('load', function () {{
    SwaggerUIBundle({{ url: '/api/openapi.json', dom_id: '#swagger' }});
  }});
</script>
</body>
</html>"#,
        title = tera::escape_html(title),
    )
}

/// Pinned, not `@latest`: a documentation page is still a page that runs
/// third-party JavaScript in a logged-in browser, and an unpinned URL is
/// whatever that CDN serves tomorrow.
const SWAGGER_CDN: &str = "https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.29.4";

/// Mounts `GET /api/openapi.json` and `GET /api/docs`.
///
/// Both are plain routes registered after the app's own, so an app that wants
/// a different docs page just claims `/api/docs` in its own `configure` — the
/// same override rule as everywhere else.
pub(crate) fn routes(docs: std::sync::Arc<ApiDocs>) -> impl Fn(&mut web::ServiceConfig) + Clone {
    move |cfg: &mut web::ServiceConfig| {
        let spec_docs = docs.clone();
        cfg.route(
            "/api/openapi.json",
            web::get().to(move |data: web::Data<crate::AppData>| {
                let docs = spec_docs.clone();
                async move { HttpResponse::Ok().json(docs.build(&data.config.base_url())) }
            }),
        );

        let page_title = docs.title.clone();
        cfg.route(
            "/api/docs",
            web::get().to(move || {
                let title = page_title.clone();
                async move {
                    HttpResponse::Ok()
                        .content_type("text/html; charset=utf-8")
                        // The app-wide policy allows scripts from `'self'`
                        // only, which would blank this page. Set here rather
                        // than widening the site policy: the exception is one
                        // route wide, visible at the place it applies, and
                        // `apply_csp` leaves a policy a handler already set.
                        .insert_header((
                            actix_web::http::header::CONTENT_SECURITY_POLICY,
                            format!(
                                "default-src 'self'; script-src 'self' {SWAGGER_CDN}; \
                                 style-src 'self' 'unsafe-inline' {SWAGGER_CDN}; \
                                 img-src 'self' data:; font-src 'self' {SWAGGER_CDN}; \
                                 connect-src 'self'; object-src 'none'; \
                                 frame-ancestors 'none'; base-uri 'self';"
                            ),
                        ))
                        .body(docs_page(&title))
                }
            }),
        );
    }
}
