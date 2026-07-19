//! The three language-switching modes over real requests: hardcoded,
//! domain-mapped, and path-prefixed (default language unprefixed, prefix
//! stripped before routing). Also covers the locale layering: framework base
//! translations < app files, per-key fallback to the default language.

use actix_web::dev::Service as _;
use actix_web::{App, HttpRequest, test, web};
use full_stack_engine::i18n::{
    LocaleSelector, apply_request_locale, build_locales, resolve_locales,
};
use full_stack_engine::prelude::tera;
use full_stack_engine::{AppData, Env, RenderTplExt};
use include_dir::{Dir, include_dir};
use std::collections::HashMap;

/// App overlay: overrides one English key, adds one app key, and partially
/// translates French (unknown to the framework) to prove per-key fallback.
static APP_LOCALES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/locales");

fn locales() -> HashMap<String, serde_json::Value> {
    resolve_locales(build_locales(Some(&APP_LOCALES)), "en")
}

fn app_data(selector: LocaleSelector) -> web::Data<AppData> {
    let mut t = tera::Tera::default();
    t.autoescape_on(vec![""]);
    t.add_raw_template("probe", "{{ t.model_ui.save }}|{{ t.app_name }}|{{ lang }}|{{ lang_prefix }}")
        .unwrap();
    web::Data::new(AppData {
        tera: t,
        db: sqlx::SqlitePool::connect_lazy("sqlite::memory:").unwrap(),
        env: Env::Prod,
        domain: String::new(),
        protocol: String::new(),
        jwt_secret: "0123456789abcdef0123456789abcdef".into(),
        smtp_from: String::new(),
        email_verification_enabled: false,
        context_injector: None,
        locales: locales(),
        locale_selector: selector,
    })
}

async fn probe(req: HttpRequest) -> actix_web::HttpResponse {
    req.render_tpl("probe", &serde_json::json!({})).await
}

macro_rules! service {
    ($selector:expr) => {{
        let selector = $selector;
        let known: Vec<String> = locales().keys().cloned().collect();
        test::init_service(
            App::new()
                .app_data(app_data(selector.clone()))
                .wrap_fn(move |mut req, srv| {
                    apply_request_locale(&selector, &known, &mut req);
                    srv.call(req)
                })
                .route("/probe", web::get().to(probe)),
        )
        .await
    }};
}

macro_rules! body_of {
    ($app:expr, $req:expr) => {{
        let res = test::call_service($app, $req.to_request()).await;
        assert!(res.status().is_success(), "status: {}", res.status());
        String::from_utf8(test::read_body(res).await.to_vec()).unwrap()
    }};
}

#[actix_web::test]
async fn hardcoded_mode_and_layering() {
    let app = service!(LocaleSelector::Hardcoded("de".into()));
    let body = body_of!(&app, test::TestRequest::get().uri("/probe"));
    // Framework German + app key falls back to English (only defined there).
    assert_eq!(body, "Speichern|Probe App|de|");
}

#[actix_web::test]
async fn app_files_override_framework_keys() {
    let app = service!(LocaleSelector::Hardcoded("en".into()));
    let body = body_of!(&app, test::TestRequest::get().uri("/probe"));
    // en.json in the fixture overrides model_ui.save.
    assert_eq!(body, "Store|Probe App|en|");
}

#[actix_web::test]
async fn domain_mode_switches_on_host() {
    let app = service!(LocaleSelector::Domain {
        map: vec![("example.de".into(), "de".into())],
        default: "en".into(),
    });
    let body = body_of!(
        &app,
        test::TestRequest::get()
            .uri("/probe")
            .insert_header(("Host", "example.de"))
    );
    assert_eq!(body, "Speichern|Probe App|de|");

    let body = body_of!(
        &app,
        test::TestRequest::get()
            .uri("/probe")
            .insert_header(("Host", "example.com"))
    );
    assert_eq!(body, "Store|Probe App|en|");
}

#[actix_web::test]
async fn path_mode_strips_prefix_and_sets_lang() {
    let app = service!(LocaleSelector::Path { default: "en".into() });

    // Default language lives unprefixed.
    let body = body_of!(&app, test::TestRequest::get().uri("/probe"));
    assert_eq!(body, "Store|Probe App|en|");

    // /de/probe routes to /probe with German + a link prefix (autoescaped
    // by Tera — HTML entities decode fine inside attribute values).
    let body = body_of!(&app, test::TestRequest::get().uri("/de/probe"));
    assert_eq!(body, "Speichern|Probe App|de|&#x2F;de");

    // Partially translated language: its keys win, gaps fall back to en.
    let body = body_of!(&app, test::TestRequest::get().uri("/fr/probe"));
    assert_eq!(body, "Enregistrer|Probe App|fr|&#x2F;fr");

    // The default language is never a prefix — /en/probe is a real 404.
    let res = test::call_service(
        &app,
        test::TestRequest::get().uri("/en/probe").to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 404);

    // Query strings survive the rewrite.
    let body = body_of!(&app, test::TestRequest::get().uri("/de/probe?x=1"));
    assert_eq!(body, "Speichern|Probe App|de|&#x2F;de");
}
