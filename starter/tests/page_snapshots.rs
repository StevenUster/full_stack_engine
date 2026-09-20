//! Snapshot tests for the pages the app renders.
//!
//! `tests/templates.rs` already proves every template *parses*. That is a
//! different question from whether a page still contains what it should: a
//! renamed context key, a changed locale file or a theme edit can leave a
//! template perfectly valid and the page quietly missing its content, and no
//! other test in this repo would notice.
//!
//! Snapshots also make the payload visible. The per-page context is serialised
//! into `__fse-props__` for client-side code, and the framework used to put
//! *every* configured language's full translations in there — 16 KB of JSON per
//! page that nothing read. A committed snapshot turns a regression like that
//! into a diff in review rather than something found with a profiler.
//!
//! `cargo insta review` to accept an intended change.

mod common;

use actix_web::{App, test};
use common::test_app_data;
use starter::serde_json;

/// Normalises everything that legitimately differs between runs, so a snapshot
/// captures the page's *shape* and nothing else.
fn stable(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    for line in html.lines() {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// The size and composition of the JSON the page ships to the client.
fn page_props(html: &str) -> serde_json::Value {
    let open = r#"<script type="application/json" id="__fse-props__">"#;
    let Some(start) = html.find(open) else {
        return serde_json::json!(null);
    };
    let rest = &html[start + open.len()..];
    let end = rest.find("</script>").expect("unterminated props block");
    let json = rest[..end].replace("\\u003c", "<");
    serde_json::from_str(&json).expect("props should be valid JSON")
}

#[actix_web::test]
async fn home_page_context_carries_only_what_it_needs() {
    let data = test_app_data().await;
    let app = test::init_service(
        App::new()
            .app_data(data.clone())
            .configure(starter::services::configure),
    )
    .await;

    let res = test::call_service(&app, test::TestRequest::get().uri("/").to_request()).await;
    assert!(res.status().is_success());
    let body = test::read_body(res).await;
    let html = String::from_utf8_lossy(&body);

    // The keys the page ships to the client, and how big each one is. `i18n`
    // (every language's translations) must not reappear here.
    let props = page_props(&html);
    let mut summary: Vec<String> = props
        .as_object()
        .expect("props should be an object")
        .iter()
        .map(|(key, value)| {
            format!(
                "{key}: {} bytes",
                serde_json::to_string(value).map_or(0, |s| s.len())
            )
        })
        .collect();
    summary.sort();
    insta::assert_snapshot!("home_page_props", summary.join("\n"));

    // Guarded separately from the snapshot so the reason is in the failure
    // message rather than only in a diff.
    assert!(
        props.get("i18n").is_none(),
        "every language's translations are back in the page payload"
    );
}

#[actix_web::test]
async fn public_product_page_renders_its_content() {
    let data = test_app_data().await;
    common::seed_product(&data, "snapshot-widget", "published").await;

    let app = test::init_service(
        App::new()
            .app_data(data.clone())
            .configure(starter::services::configure),
    )
    .await;

    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/products/snapshot-widget")
            .to_request(),
    )
    .await;
    assert!(res.status().is_success());
    let body = test::read_body(res).await;
    let html = stable(&String::from_utf8_lossy(&body));

    // The product's own data must survive the whole render pipeline.
    assert!(
        html.contains("Product snapshot-widget"),
        "product name missing"
    );
    insta::assert_snapshot!("public_product_props", {
        let props = page_props(&html);
        serde_json::to_string_pretty(&props).unwrap()
    });
}
