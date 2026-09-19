//! What the request span actually records — and, more importantly, what it
//! must never record.
//!
//! The leak these tests exist to prevent is concrete: the auth module sends
//! single-use credentials in query strings (`/reset-password?token=...`,
//! `/verify-email?token=...`), and `tracing-actix-web`'s own
//! `DefaultRootSpanBuilder` records `http.target` from `path_and_query()`. Had
//! the framework used it, every password-reset token would have been written to
//! stdout and forwarded to every telemetry backend. A unit test cannot catch
//! that, because the field is only populated by a real request through the
//! middleware — so these go through `actix_web::test` with a subscriber that
//! captures everything the stack emits.

use std::sync::{Arc, Mutex};

use actix_web::{App, HttpResponse, http::StatusCode, test, web};
use full_stack_engine::observability::FseRootSpan;
use tracing::field::{Field, Visit};
use tracing_actix_web::TracingLogger;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// A subscriber that remembers every field of every span and event
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// Every `(field, value)` pair the stack recorded, in order.
type Captured = Arc<Mutex<Vec<(String, String)>>>;

struct CaptureLayer(Captured);

struct Recorder<'a>(&'a Captured);

impl Visit for Recorder<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .lock()
            .unwrap()
            .push((field.name().to_string(), format!("{value:?}")));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.0
            .lock()
            .unwrap()
            .push((field.name().to_string(), value.to_string()));
    }
}

impl<S> Layer<S> for CaptureLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::Id,
        _ctx: Context<'_, S>,
    ) {
        attrs.record(&mut Recorder(&self.0));
    }

    fn on_record(
        &self,
        _id: &tracing::Id,
        values: &tracing::span::Record<'_>,
        _ctx: Context<'_, S>,
    ) {
        values.record(&mut Recorder(&self.0));
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        event.record(&mut Recorder(&self.0));
    }
}

/// Everything recorded while `f` ran, as `(field, value)` pairs.
async fn capture<F, Fut>(f: F) -> Vec<(String, String)>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureLayer(captured.clone()));
    let dispatch = tracing::Dispatch::new(subscriber);
    // The request runs inside the guard, so spans opened by the middleware go
    // to this subscriber rather than the process-wide one.
    let _guard = tracing::dispatcher::set_default(&dispatch);
    f().await;
    captured.lock().unwrap().clone()
}

/// The value recorded for `field`, if any.
fn field<'a>(captured: &'a [(String, String)], name: &str) -> Option<&'a str> {
    captured
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

/// Every value recorded, concatenated — for asking "does this string appear
/// *anywhere* in what we emitted?".
fn everything(captured: &[(String, String)]) -> String {
    captured
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Tests
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━

/// The whole point of `FseRootSpan`: a reset token in the query string, an
/// `Authorization` bearer token and a session cookie all pass through the
/// middleware, and none of the three may appear in anything it records.
#[actix_web::test]
async fn secrets_in_the_request_never_reach_the_telemetry() {
    const RESET_TOKEN: &str = "s3cret-reset-token";
    const BEARER: &str = "s3cret-bearer-token";
    const SESSION: &str = "s3cret-session-jwt";

    let captured = capture(|| async {
        let app = test::init_service(App::new().wrap(TracingLogger::<FseRootSpan>::new()).route(
            "/reset-password",
            web::get().to(|| async { HttpResponse::Ok().finish() }),
        ))
        .await;

        let req = test::TestRequest::get()
            .uri(&format!(
                "/reset-password?token={RESET_TOKEN}&error=expired"
            ))
            .insert_header(("authorization", format!("Bearer {BEARER}")))
            .insert_header(("cookie", format!("token={SESSION}")))
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);
    })
    .await;

    let all = everything(&captured);
    assert!(
        !all.contains(RESET_TOKEN),
        "the query string's token was recorded:\n{all}"
    );
    assert!(
        !all.contains(BEARER),
        "the Authorization header was recorded:\n{all}"
    );
    assert!(
        !all.contains(SESSION),
        "the session cookie was recorded:\n{all}"
    );
    // Not even the parameter names, since `url.path` is the path alone.
    assert!(!all.contains("token="), "a query parameter leaked:\n{all}");
    // The path itself is recorded — that is what makes the span useful.
    assert_eq!(field(&captured, "url.path"), Some("/reset-password"));
}

#[actix_web::test]
async fn a_successful_request_records_method_route_status_and_duration() {
    let captured = capture(|| async {
        let app = test::init_service(
            App::new()
                .wrap(TracingLogger::<FseRootSpan>::new())
                // A templated route, so `http.route` can be shown to be the
                // pattern rather than the concrete path.
                .route(
                    "/products/{id}",
                    web::get().to(|| async { HttpResponse::Ok().finish() }),
                ),
        )
        .await;

        let req = test::TestRequest::get().uri("/products/42").to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::OK);
    })
    .await;

    assert_eq!(field(&captured, "http.request.method"), Some("GET"));
    // The pattern, not `/products/42`: one span name per endpoint instead of
    // one per row id.
    assert_eq!(field(&captured, "http.route"), Some("/products/{id}"));
    assert_eq!(field(&captured, "url.path"), Some("/products/42"));
    assert_eq!(field(&captured, "http.response.status_code"), Some("200"));
    assert_eq!(field(&captured, "otel.status_code"), Some("OK"));
    assert_eq!(field(&captured, "otel.name"), Some("GET /products/{id}"));
    assert!(
        field(&captured, "http.server.request.duration_ms").is_some(),
        "a request duration must be recorded"
    );
    // The correlation id, and a real one rather than the "unknown" fallback.
    let request_id = field(&captured, "request_id").expect("request_id");
    assert_eq!(request_id.len(), 36, "expected a uuid: {request_id}");
    // Exactly one access line for the request.
    let access_lines = captured
        .iter()
        .filter(|(k, v)| k == "message" && v.starts_with("request "))
        .count();
    assert_eq!(access_lines, 1, "one access record per request");
}

/// A 500 is the server's fault: the span is marked failed and the cause is
/// recorded in full, chain included.
#[actix_web::test]
async fn an_internal_error_marks_the_span_failed_and_keeps_the_cause() {
    use full_stack_engine::prelude::{AppError, ErrorContext};

    let captured = capture(|| async {
        let app = test::init_service(App::new().wrap(TracingLogger::<FseRootSpan>::new()).route(
            "/boom",
            web::get().to(|| async {
                let cause = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
                Err::<HttpResponse, AppError>(
                    Err::<(), _>(cause)
                        .context("loading the price list")
                        .unwrap_err(),
                )
            }),
        ))
        .await;

        let req = test::TestRequest::get().uri("/boom").to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    })
    .await;

    assert_eq!(field(&captured, "http.response.status_code"), Some("500"));
    assert_eq!(field(&captured, "otel.status_code"), Some("ERROR"));
    // Context *and* root cause, which is what `ErrorContext` buys over
    // flattening the error into a string.
    let message = field(&captured, "exception.message").expect("exception.message");
    assert_eq!(message, "loading the price list: no such file");
    // The access line carries the same cause, so grepping the log is enough.
    assert_eq!(
        field(&captured, "cause"),
        Some("loading the price list: no such file")
    );
}

/// A 404 is ordinary traffic. It must stay visible, but it must not colour the
/// span red — otherwise a bot scanning for `/wp-login.php` looks like an
/// outage.
#[actix_web::test]
async fn a_client_error_is_recorded_without_being_marked_failed() {
    use full_stack_engine::prelude::AppError;

    let captured = capture(|| async {
        let app = test::init_service(App::new().wrap(TracingLogger::<FseRootSpan>::new()).route(
            "/missing",
            web::get().to(|| async {
                Err::<HttpResponse, AppError>(AppError::NotFound("Product 9 is gone".into()))
            }),
        ))
        .await;

        let req = test::TestRequest::get().uri("/missing").to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    })
    .await;

    assert_eq!(field(&captured, "http.response.status_code"), Some("404"));
    assert_eq!(
        field(&captured, "otel.status_code"),
        Some("OK"),
        "a client error is not a server failure"
    );
    // Still recorded, so the 404 is diagnosable.
    assert_eq!(
        field(&captured, "exception.message"),
        Some("Not Found: Product 9 is gone")
    );
}

/// An unrouted request still produces a usable span: `http.route` falls back to
/// a single low-cardinality label instead of the raw path, so scans for random
/// URLs cannot create one span name per probe.
#[actix_web::test]
async fn unmatched_requests_share_one_route_label() {
    let captured = capture(|| async {
        let app = test::init_service(App::new().wrap(TracingLogger::<FseRootSpan>::new()).route(
            "/known",
            web::get().to(|| async { HttpResponse::Ok().finish() }),
        ))
        .await;

        let req = test::TestRequest::get()
            .uri("/wp-admin/setup-config.php")
            .to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    })
    .await;

    assert_eq!(field(&captured, "http.route"), Some("unmatched"));
    // The path is still there for whoever needs to see what was probed.
    assert_eq!(
        field(&captured, "url.path"),
        Some("/wp-admin/setup-config.php")
    );
}

/// The client address comes from the rate limiter's proxy-aware resolver: the
/// *right-most* `X-Forwarded-For` entry, which a client cannot control by
/// prepending its own. `realip_remote_addr()` — what
/// `DefaultRootSpanBuilder` uses — would have recorded the spoofed one.
#[actix_web::test]
async fn client_address_ignores_a_spoofed_forwarded_for_prefix() {
    let captured = capture(|| async {
        let app = test::init_service(
            App::new()
                .wrap(TracingLogger::<FseRootSpan>::new())
                .route("/", web::get().to(|| async { HttpResponse::Ok().finish() })),
        )
        .await;

        let req = test::TestRequest::get()
            .uri("/")
            // The first entry is whatever the client claimed; the last is what
            // the trusted proxy observed.
            .insert_header(("x-forwarded-for", "9.9.9.9, 203.0.113.7"))
            .to_request();
        test::call_service(&app, req).await;
    })
    .await;

    assert_eq!(field(&captured, "client.address"), Some("203.0.113.7"));
}
