//! The batteries the framework now ships instead of every app wiring them:
//! the cross-origin policy and the published API documentation.
//!
//! Both are properties of the assembled stack rather than of a function —
//! whether a preflight is answered, and whether the docs page survives the
//! app-wide Content-Security-Policy — so they are exercised through a real
//! `App`.

use actix_web::http::header::{
    ACCESS_CONTROL_ALLOW_CREDENTIALS, ACCESS_CONTROL_ALLOW_ORIGIN, ORIGIN, VARY,
};
// `test` is aliased: importing it bare shadows the `#[test]` attribute, so the
// plain synchronous tests at the bottom of this file would not compile.
use actix_web::{App, HttpResponse, test as actix_test, web};
use full_stack_engine::config::CorsConfig;
use full_stack_engine::models::openapi::ApiDocs;

fn header<B>(
    res: &actix_web::dev::ServiceResponse<B>,
    name: actix_web::http::header::HeaderName,
) -> String {
    res.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string()
}

macro_rules! cors_app {
    ($cors:expr) => {
        actix_test::init_service(
            App::new()
                .wrap(full_stack_engine::cors_middleware(&$cors))
                .route(
                    "/api/x",
                    web::get().to(|| async { HttpResponse::Ok().json(1) }),
                ),
        )
        .await
    };
}

#[actix_web::test]
async fn disabled_is_the_default_and_emits_no_cross_origin_headers() {
    let app = cors_app!(CorsConfig::Disabled);
    let req = actix_test::TestRequest::get()
        .uri("/api/x")
        .insert_header((ORIGIN, "https://evil.example"))
        .to_request();
    let res = actix_test::call_service(&app, req).await;
    // Not merely absent from the allow-list — no header at all, so the
    // browser's same-origin rule applies untouched.
    assert!(
        !res.headers().contains_key(ACCESS_CONTROL_ALLOW_ORIGIN),
        "an app that never configured CORS must not answer cross-origin"
    );
}

#[actix_web::test]
async fn any_allows_every_origin_but_never_credentials() {
    let app = cors_app!(CorsConfig::Any);
    for origin in ["https://anywhere.example", "http://localhost:5173"] {
        let req = actix_test::TestRequest::get()
            .uri("/api/x")
            .insert_header((ORIGIN, origin))
            .to_request();
        let res = actix_test::call_service(&app, req).await;
        // The requesting origin is echoed rather than a literal `*`. Both
        // permit the read; echoing additionally keeps the response correct for
        // caches, which is why it comes with `Vary: Origin`.
        assert_eq!(header(&res, ACCESS_CONTROL_ALLOW_ORIGIN), origin);
        assert!(header(&res, VARY).contains("Origin"));
        // The combination browsers reject, and which would otherwise let any
        // site read a logged-in user's data.
        assert!(
            !res.headers().contains_key(ACCESS_CONTROL_ALLOW_CREDENTIALS),
            "an any-origin policy must never carry credentials"
        );
    }
}

#[actix_web::test]
async fn a_named_origin_is_echoed_with_vary_and_may_send_cookies() {
    let app = cors_app!(CorsConfig::Origins(vec!["https://app.example".to_string()]));

    let allowed = actix_test::TestRequest::get()
        .uri("/api/x")
        .insert_header((ORIGIN, "https://app.example"))
        .to_request();
    let res = actix_test::call_service(&app, allowed).await;
    assert_eq!(
        header(&res, ACCESS_CONTROL_ALLOW_ORIGIN),
        "https://app.example"
    );
    assert_eq!(header(&res, ACCESS_CONTROL_ALLOW_CREDENTIALS), "true");
    // Without `Vary: Origin` a shared cache can serve one origin's allowed
    // response to another — the bug the hand-rolled `*` header had.
    assert!(
        header(&res, VARY).contains("Origin"),
        "missing Vary: Origin — a cache could leak the allowance"
    );

    let other = actix_test::TestRequest::get()
        .uri("/api/x")
        .insert_header((ORIGIN, "https://evil.example"))
        .to_request();
    let res = actix_test::call_service(&app, other).await;
    assert!(
        !res.headers().contains_key(ACCESS_CONTROL_ALLOW_ORIGIN),
        "an unlisted origin must not be allowed"
    );
}

#[actix_web::test]
async fn a_preflight_is_answered_without_reaching_the_handler() {
    let app = actix_test::init_service(
        App::new()
            .wrap(full_stack_engine::cors_middleware(&CorsConfig::Origins(
                vec!["https://app.example".to_string()],
            )))
            .route(
                "/api/x",
                // A preflight that fell through to routing would 405 here,
                // since only GET is mounted.
                web::get().to(|| async { HttpResponse::Ok().json(1) }),
            ),
    )
    .await;

    let req = actix_test::TestRequest::default()
        .method(actix_web::http::Method::OPTIONS)
        .uri("/api/x")
        .insert_header((ORIGIN, "https://app.example"))
        .insert_header((
            actix_web::http::header::ACCESS_CONTROL_REQUEST_METHOD,
            "POST",
        ))
        .to_request();
    let res = actix_test::call_service(&app, req).await;
    assert!(
        res.status().is_success(),
        "preflight was not handled: {}",
        res.status()
    );
    assert_eq!(
        header(&res, ACCESS_CONTROL_ALLOW_ORIGIN),
        "https://app.example"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// API documentation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn hand_written_paths_merge_over_the_generated_document() {
    let docs = ApiDocs::new("Example API", "1.2.3", "Public data.")
        .paths(serde_json::json!({
            "/api/events": {
                "get": { "summary": "List events", "responses": { "200": { "description": "ok" } } }
            }
        }))
        .schemas(serde_json::json!({
            "Event": { "type": "object", "properties": { "id": { "type": "integer" } } }
        }));

    let doc = docs.build("https://example.com");

    assert_eq!(doc["openapi"], "3.0.3");
    assert_eq!(doc["info"]["title"], "Example API");
    assert_eq!(doc["info"]["version"], "1.2.3");
    assert_eq!(doc["servers"][0]["url"], "https://example.com");

    // The hand-written route is present...
    assert_eq!(doc["paths"]["/api/events"]["get"]["summary"], "List events");
    assert!(doc["components"]["schemas"]["Event"].is_object());
    // ...alongside what the generator always emits, so a `$ref` to either
    // half cannot dangle.
    assert!(doc["components"]["schemas"]["Error"].is_object());
    assert_eq!(
        doc["components"]["securitySchemes"]["sessionCookie"]["in"],
        "cookie"
    );
}

#[test]
fn a_document_with_no_hand_written_paths_is_still_valid() {
    let doc = ApiDocs::new("API", "0.1.0", "").build("https://example.com");
    assert!(doc["paths"].is_object());
    assert!(doc["components"]["schemas"]["Error"].is_object());
}
