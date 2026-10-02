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

use full_stack_engine::testing::TestApp;
use starter::models::product::{Product, ProductStatus};
use starter::{insert, serde_json};

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
    // The production stack stamps a CSP nonce onto every <script>, so the
    // tag is matched by its id rather than verbatim.
    let Some(start) = html.find(r#"id="__fse-props__""#) else {
        return serde_json::json!(null);
    };
    let rest = &html[start..];
    let rest = &rest[rest.find('>').expect("unterminated props tag") + 1..];
    let end = rest.find("</script>").expect("unterminated props block");
    let json = rest[..end].replace("\\u003c", "<");
    serde_json::from_str(&json).expect("props should be valid JSON")
}

#[actix_web::test]
async fn home_page_context_carries_only_what_it_needs() {
    let app = TestApp::new(starter::app()).await;
    let res = app.get("/").send().await;
    assert_eq!(res.status, 200);
    let html = res.body;

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
    let app = TestApp::new(starter::app()).await;
    insert!(
        Product,
        &app.db,
        name = "Product snapshot-widget".to_string(),
        slug = "snapshot-widget".to_string(),
        price = 9.99,
        status = ProductStatus::Published
    )
    .await
    .unwrap();

    let res = app.get("/products/snapshot-widget").send().await;
    assert_eq!(res.status, 200);
    let html = stable(&res.body);

    // The product's own data must survive the whole render pipeline.
    assert!(
        html.contains("Product snapshot-widget"),
        "product name missing"
    );
    let mut props = page_props(&html);
    // The creation time is the only value that differs between runs.
    for key in ["created_at", "created_at_display"] {
        props["row"][key] = serde_json::json!("[timestamp]");
    }
    // Anonymous pages must never learn the admin side of a model.
    assert!(props["meta"].get("form_columns").is_none());
    assert_ne!(props["meta"]["base_path"], "/admin/products");
    insta::assert_snapshot!(
        "public_product_props",
        serde_json::to_string_pretty(&props).unwrap()
    );
}
