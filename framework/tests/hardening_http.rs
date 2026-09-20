//! The response pipeline the framework wraps around every app: compression,
//! asset caching, the per-request CSP nonce, and the health probes.
//!
//! These go through `actix_web::test` against a real `App` because each of them
//! is a property of the *assembled* middleware stack — the order matters (the
//! nonce must be stamped on the body before compression sees it, and the policy
//! must not overwrite the stricter one the uploads mount sets), and a unit test
//! on any single function cannot catch a mis-ordering.

use actix_web::dev::Service as _;
use actix_web::http::header::{
    ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_SECURITY_POLICY, CONTENT_TYPE,
};
use actix_web::{App, HttpResponse, test, web};
use full_stack_engine::{Env, ScriptNonce};

/// A page with an inline script, the shape Astro emits for hydration.
const PAGE: &str = "<html><head><script>hydrate()</script></head><body>hi</body></html>";

/// Mirrors the framework's own stack for the parts under test. Kept explicit
/// rather than booting `FrameworkApp`, which would need a database, a theme and
/// a validated environment.
macro_rules! csp_app {
    ($env:expr) => {
        test::init_service(
            App::new()
                .wrap(actix_web::middleware::Compress::default())
                .wrap_fn(move |req, srv| {
                    use actix_web::HttpMessage as _;
                    let nonce = ScriptNonce::generate();
                    req.extensions_mut().insert(nonce.clone());
                    let fut = srv.call(req);
                    async move { full_stack_engine::apply_csp($env, nonce, fut.await?).await }
                })
                .route(
                    "/page",
                    web::get()
                        .to(|| async { HttpResponse::Ok().content_type("text/html").body(PAGE) }),
                )
                .route(
                    "/data.json",
                    web::get().to(|| async {
                        HttpResponse::Ok()
                            .content_type("application/json")
                            .body(r#"{"a":"<script>"}"#)
                    }),
                )
                // Something that already declares its own, stricter policy —
                // the uploads mount does exactly this.
                .route(
                    "/sandboxed",
                    web::get().to(|| async {
                        HttpResponse::Ok()
                            .content_type("text/html")
                            .insert_header((CONTENT_SECURITY_POLICY, "sandbox"))
                            .body(PAGE)
                    }),
                ),
        )
        .await
    };
}

fn body_of(bytes: &actix_web::web::Bytes) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[actix_web::test]
async fn production_pages_get_a_nonce_in_both_the_policy_and_the_body() {
    let app = csp_app!(Env::Prod);
    let res = test::call_service(&app, test::TestRequest::get().uri("/page").to_request()).await;

    let policy = res
        .headers()
        .get(CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let body = body_of(&test::read_body(res).await);

    // The nonce in the header and the nonce in the body must be the same value,
    // or every inline script on the page is refused by the browser.
    let nonce = policy
        .split("'nonce-")
        .nth(1)
        .and_then(|rest| rest.split('\'').next())
        .expect("policy should carry a nonce")
        .to_string();
    assert_eq!(nonce.len(), 32);
    assert!(
        body.contains(&format!(r#"<script nonce="{nonce}">"#)),
        "inline script was not stamped with the policy's nonce:\n{body}"
    );
    // And the escape hatch the nonce replaces is gone.
    assert!(
        !policy.contains("script-src 'self' 'unsafe-inline'"),
        "{policy}"
    );
}

#[actix_web::test]
async fn two_requests_never_share_a_nonce() {
    let app = csp_app!(Env::Prod);
    let mut seen = Vec::new();
    for _ in 0..3 {
        let res =
            test::call_service(&app, test::TestRequest::get().uri("/page").to_request()).await;
        seen.push(
            res.headers()
                .get(CONTENT_SECURITY_POLICY)
                .unwrap()
                .to_str()
                .unwrap()
                .to_string(),
        );
    }
    seen.dedup();
    // A nonce reused across requests is guessable from any one response, which
    // would make the policy no stronger than 'unsafe-inline'.
    assert_eq!(seen.len(), 3, "nonces were reused across requests");
}

#[actix_web::test]
async fn development_keeps_inline_scripts_working_for_hmr() {
    let app = csp_app!(Env::Dev);
    let res = test::call_service(&app, test::TestRequest::get().uri("/page").to_request()).await;
    let policy = res
        .headers()
        .get(CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    // Dev proxies pages straight from the theme's dev server, which never pass
    // through the renderer, so nonces would break HMR instead of securing it.
    assert!(policy.contains("'unsafe-inline'"), "{policy}");
    assert!(policy.contains("'unsafe-eval'"), "{policy}");
    assert!(!policy.contains("nonce-"), "{policy}");
}

#[actix_web::test]
async fn a_stricter_policy_set_by_a_handler_is_not_overwritten() {
    let app = csp_app!(Env::Prod);
    let res = test::call_service(
        &app,
        test::TestRequest::get().uri("/sandboxed").to_request(),
    )
    .await;
    // The uploads mount serves user-supplied files under `sandbox`; replacing
    // that with the site-wide policy would let an uploaded HTML file run script
    // in the site's origin — stored XSS.
    assert_eq!(
        res.headers().get(CONTENT_SECURITY_POLICY).unwrap(),
        "sandbox"
    );
}

#[actix_web::test]
async fn non_html_bodies_are_never_rewritten() {
    let app = csp_app!(Env::Prod);
    let res = test::call_service(
        &app,
        test::TestRequest::get().uri("/data.json").to_request(),
    )
    .await;
    assert_eq!(res.headers().get(CONTENT_TYPE).unwrap(), "application/json");
    let body = body_of(&test::read_body(res).await);
    // A JSON string containing "<script" must come back byte-identical.
    assert_eq!(body, r#"{"a":"<script>"}"#);
}

#[actix_web::test]
async fn responses_are_compressed_when_the_client_asks() {
    let app = csp_app!(Env::Prod);
    // Compression needs a body big enough to be worth it, so send a real one.
    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/page")
            .insert_header((ACCEPT_ENCODING, "gzip"))
            .to_request(),
    )
    .await;
    assert_eq!(
        res.headers()
            .get(CONTENT_ENCODING)
            .map(|v| v.to_str().unwrap()),
        Some("gzip"),
        "nothing was compressed; a 77 KB page used to go out uncompressed"
    );
}

#[actix_web::test]
async fn an_unencoded_client_still_gets_a_readable_body() {
    let app = csp_app!(Env::Prod);
    let res = test::call_service(&app, test::TestRequest::get().uri("/page").to_request()).await;
    assert!(res.headers().get(CONTENT_ENCODING).is_none());
    assert!(body_of(&test::read_body(res).await).contains("hydrate()"));
}

/// The asset cache policy, checked through the framework's own asset service so
/// the header and the fingerprint rule are tested together.
#[actix_web::test]
async fn fingerprinted_assets_are_cached_forever_and_others_are_not() {
    use full_stack_engine::themes::{Theme, ThemeManifest};

    let theme = Theme::new(ThemeManifest {
        name: "t".into(),
        parent: None,
        version: None,
        description: None,
    })
    .with_file("index.html", "hi")
    .with_file("_astro/app.CwkyWajQ.css", "body{}")
    .with_file("favicon.ico", "x");
    let stack = std::sync::Arc::new(full_stack_engine::testing::theme_stack([theme]).unwrap());

    let assets = stack.clone();
    let app = test::init_service(App::new().default_service(web::to(
        move |req: actix_web::HttpRequest| {
            let assets = assets.clone();
            async move {
                full_stack_engine::serve_asset(
                    &assets,
                    req.path().trim_start_matches('/'),
                    req.method().as_str(),
                )
            }
        },
    )))
    .await;

    // Hashed: the URL changes when the bytes do, so a year is safe and saves
    // re-downloading a stylesheet on every navigation.
    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/_astro/app.CwkyWajQ.css")
            .to_request(),
    )
    .await;
    let cache = res.headers().get(CACHE_CONTROL).unwrap().to_str().unwrap();
    assert!(cache.contains("max-age=31536000"), "{cache}");
    assert!(cache.contains("immutable"), "{cache}");

    // Unhashed: same URL forever, so it must revalidate or an edit never ships.
    let res = test::call_service(
        &app,
        test::TestRequest::get().uri("/favicon.ico").to_request(),
    )
    .await;
    let cache = res.headers().get(CACHE_CONTROL).unwrap().to_str().unwrap();
    assert!(cache.contains("must-revalidate"), "{cache}");
    assert!(!cache.contains("immutable"), "{cache}");
}

/// The probes a container runtime needs. `/healthz` must not depend on the
/// database: a failing database should take the instance out of rotation
/// (`/readyz`), not get the container killed and restarted, which fixes
/// nothing and loses the in-flight requests too.
#[actix_web::test]
async fn health_and_readiness_probes_answer_separately() {
    use full_stack_engine::themes::{Theme, ThemeManifest};

    let theme = Theme::new(ThemeManifest {
        name: "t".into(),
        parent: None,
        version: None,
        description: None,
    })
    .with_file("index.html", "hi");

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let data = full_stack_engine::testing::app_data(pool.clone(), [theme], "test-secret-value");

    let app = test::init_service(
        App::new()
            .app_data(web::Data::new(data))
            .configure(full_stack_engine::health_routes)
            // A neighbouring route, because the first attempt registered the
            // probes as a `web::scope("")` — which matched every path and
            // answered 404 for everything else, taking the whole app down. A
            // test with only the probes registered could not see that.
            .route(
                "/",
                web::get().to(|| async { HttpResponse::Ok().body("home") }),
            ),
    )
    .await;

    let res = test::call_service(&app, test::TestRequest::get().uri("/").to_request()).await;
    assert!(
        res.status().is_success(),
        "registering the health probes must not shadow the app's own routes"
    );

    let res = test::call_service(&app, test::TestRequest::get().uri("/healthz").to_request()).await;
    assert!(res.status().is_success());
    assert_eq!(body_of(&test::read_body(res).await), "ok");

    let res = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert!(res.status().is_success());
    assert_eq!(body_of(&test::read_body(res).await), "ready");

    // With the pool gone, liveness still passes and readiness fails — which is
    // the whole point of having two.
    pool.close().await;
    let res = test::call_service(&app, test::TestRequest::get().uri("/healthz").to_request()).await;
    assert!(
        res.status().is_success(),
        "liveness must not depend on the database"
    );

    let res = test::call_service(&app, test::TestRequest::get().uri("/readyz").to_request()).await;
    assert_eq!(
        res.status(),
        actix_web::http::StatusCode::SERVICE_UNAVAILABLE
    );
}
