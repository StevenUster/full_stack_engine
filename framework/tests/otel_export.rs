//! What an OTLP backend actually receives, checked by exporting through a real
//! `OpenTelemetry` pipeline into memory.
//!
//! The other observability tests capture `tracing` fields, which is not the
//! same thing: `tracing-opentelemetry` decides what becomes the exported
//! span's name and status, and it can ignore a field the `tracing` side
//! recorded perfectly well. That gap is where request spans were all exported
//! as `http_request`.
#![cfg(feature = "otel")]

use actix_web::{App, HttpResponse, test as actix_test, web};
use full_stack_engine::observability::FseRootSpan;
use opentelemetry::trace::{Status, TracerProvider as _};
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tracing_actix_web::TracingLogger;
use tracing_subscriber::layer::SubscriberExt as _;

/// Runs `requests` through a real `TracingLogger<FseRootSpan>` and returns
/// every span the OpenTelemetry pipeline exported.
async fn export(requests: &[&str]) -> Vec<SpanData> {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _default = tracing::subscriber::set_default(subscriber);

    let app = actix_test::init_service(
        App::new()
            .wrap(TracingLogger::<FseRootSpan>::new())
            .route(
                "/products/{id}",
                web::get().to(|| async { HttpResponse::Ok().finish() }),
            )
            .route(
                "/broken",
                web::get().to(|| async { HttpResponse::InternalServerError().finish() }),
            ),
    )
    .await;
    for uri in requests {
        let req = actix_test::TestRequest::get().uri(uri).to_request();
        let _ = actix_test::call_service(&app, req).await;
    }

    provider.force_flush().unwrap();
    exporter.get_finished_spans().unwrap()
}

#[actix_web::test]
async fn a_request_span_is_exported_under_its_route_not_a_generic_name() {
    let spans = export(&["/products/42"]).await;
    let names: Vec<&str> = spans.iter().map(|s| s.name.as_ref()).collect();
    // The pattern, not the path: one series per route, not one per product.
    assert_eq!(names, ["GET /products/{id}"], "exported span names");
}

#[actix_web::test]
async fn unmatched_paths_share_one_name() {
    let spans = export(&["/scanner/probe-1", "/scanner/probe-2"]).await;
    for span in &spans {
        assert_eq!(span.name.as_ref(), "GET unmatched");
    }
    assert_eq!(spans.len(), 2);
}

#[actix_web::test]
async fn a_5xx_is_exported_as_an_error_and_a_success_is_not() {
    let spans = export(&["/broken", "/products/1"]).await;
    let status_of = |name: &str| {
        spans
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.status.clone())
            .unwrap_or_else(|| panic!("no span named {name}: {spans:?}"))
    };
    assert!(
        matches!(status_of("GET /broken"), Status::Error { .. }),
        "a 5xx must reach the backend as an error"
    );
    assert_eq!(status_of("GET /products/{id}"), Status::Ok);
}
