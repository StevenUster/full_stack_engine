//! What the sidebar is generated from: `nav(...)` / `nav = false` /
//! `public_nav(...)` and `ModelHooks::in_nav` decide the entries, the page's
//! translations their labels; nested models never get one. Plus the public
//! list's own default order (`public_order_by`) and page-context providers
//! (`FrameworkApp::page_context`), which put app data into pages the app
//! doesn't render itself.

#![deny(warnings)]
#![allow(dead_code)]

use actix_web::cookie::Cookie;
use actix_web::{App, HttpRequest, HttpResponse, test, web};
use full_stack_engine::auth::create_jwt;
use full_stack_engine::models::{self, AppResult, CurrentUser, ModelHooks};
use full_stack_engine::prelude::{RenderTplExt, model, tera};
use full_stack_engine::structs::User;
use full_stack_engine::{AppData, Env, PageContexts, define_roles};
use serde_json::{Value, json};
use sqlx::SqlitePool;

define_roles! {
    (Admin, "admin", ["all"]),
    (Writer, "writer", [
        "nav_articles.read",
        "nav_diaries.read",
        "nav_plain_items.read",
        "nav_hidden_notes.read",
    ]),
    (None, "none", ["none"]),
}

/// Public list in the sidebar for everyone, admin list for readers.
#[model(
    path = "articles",
    public_read = slug,
    order_by = "-id",
    public_order_by = "title",
    public_nav(order = 1, icon = "file"),
    nav(order = 3, icon = "list")
)]
struct NavArticle {
    id: i64,
    title: String,
    #[orm(unique)]
    slug: String,
}

/// Nested: reached from its article, never from the sidebar.
#[model(parent = article_id)]
struct NavComment {
    id: i64,
    #[orm(references(NavArticle, on_delete = cascade))]
    article_id: i64,
    body: String,
}

/// A "my ..." page that `in_nav` keeps out of the admin's sidebar.
#[model(path = "my-diary", nav(order = 2, icon = "heart"), hooks)]
struct NavDiary {
    id: i64,
    entry: String,
}

impl ModelHooks for NavDiary {
    fn in_nav(user: &CurrentUser) -> bool {
        !user.is_admin()
    }
}

#[model(nav = false)]
struct NavHiddenNote {
    id: i64,
    text: String,
}

/// No attributes: an entry after the ordered ones, labelled from its name.
#[model]
struct NavPlainItem {
    id: i64,
    name: String,
}

const SECRET: &str = "0123456789abcdef0123456789abcdef";

fn app_data(db: SqlitePool) -> web::Data<AppData> {
    let mut t = tera::Tera::default();
    for name in ["fse/list", "fse/public-list", "page"] {
        t.add_raw_template(name, "{{ __tera_context | safe }}")
            .unwrap();
    }
    web::Data::new(AppData {
        tera: t,
        db,
        env: Env::Prod,
        domain: String::new(),
        protocol: String::new(),
        config: std::sync::Arc::new(full_stack_engine::testing::config(SECRET)),
        smtp_from: String::new(),
        email_verification_enabled: false,
        context_injector: None,
        locales: std::collections::HashMap::new(),
        locale_selector: full_stack_engine::i18n::LocaleSelector::Hardcoded("en".into()),
        themes: std::sync::Arc::default(),
    })
}

fn cookie(id: i64, role: AppRole) -> Cookie<'static> {
    let user = User::<AppRole> {
        id,
        email: format!("nav-{id}@test.dev"),
        password: String::new(),
        role,
        created_at: chrono::Utc::now().naive_utc(),
        is_verified: true,
        verification_token: None,
    };
    Cookie::new("token", create_jwt(&user, SECRET).unwrap())
}

/// The `nav` a page gets for `cookie` (`None`: signed out), with `t` as
/// the page's translations.
fn nav_for(data: &web::Data<AppData>, cookie: Option<Cookie<'static>>, t: &Value) -> Value {
    let mut req = test::TestRequest::default().app_data(data.clone());
    if let Some(c) = cookie {
        req = req.cookie(c);
    }
    let mut value = json!({ "t": t });
    models::inject_nav::<AppRole>(&req.to_http_request(), &mut value);
    value
}

fn labels(value: &Value) -> Vec<String> {
    value["nav"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["label"].as_str().unwrap().to_string())
        .collect()
}

#[actix_web::test]
async fn nav_entries_come_from_the_model_attributes() {
    let db = SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    let data = app_data(db);
    let t = json!({ "models": {
        "nav_articles": { "title": "Articles", "nav": "Write", "public_nav": "Read" },
        "nav_diaries": { "title": "My diary" },
    }});

    // Signed out: only the public list, and no `user`.
    let anon = nav_for(&data, None, &t);
    assert_eq!(labels(&anon), ["Read"]);
    assert_eq!(anon["nav"][0]["href"], "/nav_articles");
    assert_eq!(anon["nav"][0]["icon"], "file");
    assert_eq!(anon["nav"][0]["public"], true);
    assert!(anon.get("user").is_none());

    // A reader: ordered entries first, then the rest by table name; the
    // label falls back to `title`, then to the readable table name. No
    // nested model, nothing marked `nav = false`.
    let writer = nav_for(&data, Some(cookie(1, AppRole::Writer)), &t);
    assert_eq!(
        labels(&writer),
        ["Read", "My diary", "Write", "Nav plain items"]
    );
    let entries = writer["nav"].as_array().unwrap();
    assert_eq!(entries[1]["href"], "/my-diary");
    assert_eq!(entries[1]["icon"], "heart");
    assert_eq!(entries[2]["href"], "/articles");
    assert_eq!(entries[2]["public"], false);
    assert_eq!(entries[3]["icon"], "table", "the theme's default icon");
    assert_eq!(writer["user"]["role"], "writer");

    // `in_nav` hides the diary from admins, who could read it.
    let admin = nav_for(&data, Some(cookie(2, AppRole::Admin)), &t);
    assert_eq!(labels(&admin), ["Read", "Write", "Nav plain items"]);
}

#[actix_web::test]
async fn the_public_list_has_its_own_default_order() {
    let db = SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    sqlx::query("DELETE FROM nav_articles")
        .execute(&db)
        .await
        .unwrap();
    for (title, slug) in [("Beta", "b"), ("Alpha", "a"), ("Gamma", "c")] {
        sqlx::query("INSERT INTO nav_articles (title, slug) VALUES (?, ?)")
            .bind(title)
            .bind(slug)
            .execute(&db)
            .await
            .unwrap();
    }
    // Generated endpoints check the session against the users table.
    let admin_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, password, sessions_valid_after) VALUES ('nav-admin@test.dev', 'x', 0) \
         ON CONFLICT(email) DO UPDATE SET password = 'x' RETURNING id",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    let app = test::init_service(
        App::new()
            .app_data(app_data(db))
            .configure(models::mount_all::<AppRole>),
    )
    .await;

    let titles = |ctx: Value| -> Vec<String> {
        ctx["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["title"].as_str().unwrap().to_string())
            .collect()
    };
    let public = test::call_and_read_body(
        &app,
        test::TestRequest::get().uri("/nav_articles").to_request(),
    )
    .await;
    assert_eq!(
        titles(serde_json::from_slice(&public).unwrap()),
        ["Alpha", "Beta", "Gamma"]
    );
    let admin = test::call_and_read_body(
        &app,
        test::TestRequest::get()
            .uri("/articles")
            .cookie(cookie(admin_id, AppRole::Admin))
            .to_request(),
    )
    .await;
    assert_eq!(
        titles(serde_json::from_slice(&admin).unwrap()),
        ["Gamma", "Alpha", "Beta"],
        "the admin list keeps order_by = \"-id\""
    );
}

async fn render_page(req: HttpRequest) -> HttpResponse {
    let explicit: Value = match req.query_string() {
        "error" => json!({ "error": "boom", "who": "handler" }),
        _ => json!({}),
    };
    req.render_tpl("page", &explicit).await
}

async fn provider_a(_req: HttpRequest) -> AppResult<Value> {
    Ok(json!({ "who": "a", "a": 1, "error": "" }))
}

async fn provider_b(req: HttpRequest) -> AppResult<Value> {
    Ok(json!({ "who": "b", "path": req.path() }))
}

#[actix_web::test]
async fn page_contexts_lay_the_base_under_the_handlers_context() {
    let db = SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    let mut contexts = PageContexts::default();
    contexts.add("page", provider_a);
    contexts.add("page", provider_b);
    contexts.add("other", |_req: HttpRequest| async {
        Ok(json!({ "never": true }))
    });
    let app = test::init_service(
        App::new()
            .app_data(app_data(db))
            .app_data(web::Data::new(contexts))
            .route("/page", web::get().to(render_page)),
    )
    .await;

    let body =
        test::call_and_read_body(&app, test::TestRequest::get().uri("/page").to_request()).await;
    let ctx: Value = serde_json::from_slice(&body).unwrap();
    // Every provider of the template, later ones winning on a shared key.
    assert_eq!(ctx["a"], 1);
    assert_eq!(ctx["who"], "b");
    assert_eq!(ctx["path"], "/page");
    assert!(
        ctx.get("never").is_none(),
        "only the template's own providers run"
    );

    // The handler's own keys win over every provider.
    let body = test::call_and_read_body(
        &app,
        test::TestRequest::get().uri("/page?error").to_request(),
    )
    .await;
    let ctx: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(ctx["who"], "handler");
    assert_eq!(ctx["error"], "boom");
    assert_eq!(ctx["a"], 1);
}
