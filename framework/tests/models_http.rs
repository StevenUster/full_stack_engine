//! End-to-end HTTP tests for the generated CRUD routes: this test crate
//! plays the role of an app — `#[model]` structs + `define_roles!` — and
//! drives the mounted routes with real requests: permissions, forms with
//! validation errors, redirects, public pages, the JSON API, template
//! override preference, and app-route shadowing.

#![allow(dead_code)]

use actix_web::cookie::Cookie;
use actix_web::http::header::LOCATION;
use actix_web::{App, HttpResponse, test, web};
use chrono::NaiveDateTime;
use fse_orm::DbEnum;
use full_stack_engine::auth::create_jwt;
use full_stack_engine::models;
use full_stack_engine::prelude::{Role, model, tera};
use full_stack_engine::structs::User;
use full_stack_engine::{AppData, Env, define_roles};
use sqlx::SqlitePool;

define_roles! {
    (Admin, "admin", ["all"]),
    (Editor, "editor", ["content.read", "content.write"]),
    (Viewer, "viewer", ["content.read"]),
    (None, "none", ["none"]),
}

#[derive(DbEnum, Debug, Clone, Copy, PartialEq, Eq)]
enum PostStatus {
    Draft,
    Published,
}

#[model(permission = "content", public_read = slug, api)]
struct Post {
    id: i64,
    #[ui(list, search)]
    title: String,
    #[orm(unique)]
    #[ui(list)]
    slug: String,
    #[orm(default = "draft")]
    #[ui(list, filter)]
    status: PostStatus,
    #[orm(default = now)]
    created_at: NaiveDateTime,
}

/// The `AuthUser` extractor validates sessions against a `users` table, so
/// the "app" defines one. `disabled` — auth pages aren't under test.
#[model(disabled)]
#[orm(table = "users")]
struct AppUser {
    id: i64,
    #[orm(unique)]
    email: String,
    password: String,
    #[orm(default = 0)]
    sessions_valid_after: i64,
}

const SECRET: &str = "0123456789abcdef0123456789abcdef";

fn test_tera() -> tera::Tera {
    let mut t = tera::Tera::default();
    t.autoescape_on(vec![""]);
    // Stand-ins for the theme's generic templates (phase 3) — they dump just
    // enough context to assert on.
    t.add_raw_template(
        "fse/list",
        "LIST {{ meta.table }} n={{ rows | length }} total={{ total }}",
    )
    .unwrap();
    t.add_raw_template(
        "fse/form",
        "FORM {{ meta.table }} errors={{ errors | length }} new={{ is_new }}",
    )
    .unwrap();
    t.add_raw_template("fse/public-list", "PUB {{ meta.table }} total={{ total }}")
        .unwrap();
    t.add_raw_template("fse/public-detail", "PUBDET {{ row.slug }}")
        .unwrap();
    // A model-specific template: must win over fse/public-list.
    t.add_raw_template("posts", "PUB-OVERRIDE total={{ total }}")
        .unwrap();
    t
}

fn app_data(db: SqlitePool) -> web::Data<AppData> {
    web::Data::new(AppData {
        tera: test_tera(),
        db,
        env: Env::Prod,
        domain: String::new(),
        protocol: String::new(),
        jwt_secret: SECRET.to_string(),
        smtp_from: String::new(),
        email_verification_enabled: false,
        context_injector: None,
        locales: std::collections::HashMap::new(),
        locale_selector: full_stack_engine::i18n::LocaleSelector::default(),
    })
}

async fn token(db: &SqlitePool, role: AppRole) -> Cookie<'static> {
    let email = format!("{}@test.dev", Role::as_str(&role));
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO users (email, password, sessions_valid_after) VALUES (?, 'x', 0) \
         ON CONFLICT(email) DO UPDATE SET password = 'x' RETURNING id",
    )
    .bind(&email)
    .fetch_one(db)
    .await
    .unwrap();
    let user = User::<AppRole> {
        id,
        email,
        password: String::new(),
        role,
        created_at: chrono::Utc::now().naive_utc(),
        is_verified: true,
        verification_token: None,
    };
    Cookie::new("token", create_jwt(&user, SECRET).unwrap())
}

#[actix_web::test]
async fn generated_crud_over_http() {
    let db = SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    Post::delete_where().execute(&db).await.unwrap();
    let editor = token(&db, AppRole::Editor).await;
    let viewer = token(&db, AppRole::Viewer).await;

    let app = test::init_service(
        App::new()
            .app_data(app_data(db.clone()))
            .configure(models::mount_all::<AppRole>),
    )
    .await;

    // No token: the admin UI redirects to login.
    let res = test::call_service(&app, test::TestRequest::get().uri("/admin/posts").to_request()).await;
    assert_eq!(res.status().as_u16(), 302);
    assert_eq!(res.headers().get(LOCATION).unwrap(), "/login");

    // Read permission suffices for the list.
    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/admin/posts")
            .cookie(viewer.clone())
            .to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 200);
    let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    assert_eq!(body, "LIST posts n=0 total=0");

    // ...but not for the create form.
    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/admin/posts/create")
            .cookie(viewer.clone())
            .to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 401);

    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/admin/posts/create")
            .cookie(editor.clone())
            .to_request(),
    )
    .await;
    let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    assert_eq!(body, "FORM posts errors=0 new=true");

    // Invalid submit re-renders the form with the collected errors.
    let res = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/admin/posts/create")
            .cookie(editor.clone())
            .set_form([("title", ""), ("slug", "first"), ("status", "draft")])
            .to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 200);
    let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    assert_eq!(body, "FORM posts errors=1 new=true");

    // Valid submit redirects to the new row.
    let res = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/admin/posts/create")
            .cookie(editor.clone())
            .set_form([("title", "First"), ("slug", "first"), ("status", "draft")])
            .to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 302);
    let location = res.headers().get(LOCATION).unwrap().to_str().unwrap().to_string();
    assert!(location.starts_with("/admin/posts/"), "{location}");
    let id: i64 = location.rsplit('/').next().unwrap().parse().unwrap();

    // Edit form renders the row; unknown id is 404.
    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&location)
            .cookie(editor.clone())
            .to_request(),
    )
    .await;
    let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    assert_eq!(body, "FORM posts errors=0 new=false");
    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/admin/posts/999999")
            .cookie(editor.clone())
            .to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 404);

    // Update publishes the post.
    let res = test::call_service(
        &app,
        test::TestRequest::post()
            .uri(&location)
            .cookie(editor.clone())
            .set_form([("title", "First!"), ("slug", "first"), ("status", "published")])
            .to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 302);

    // Public pages need no auth; the model-specific "posts" template wins
    // over the generic fse/public-list.
    let res = test::call_service(&app, test::TestRequest::get().uri("/posts").to_request()).await;
    assert_eq!(res.status().as_u16(), 200);
    let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    assert_eq!(body, "PUB-OVERRIDE total=1");

    let res =
        test::call_service(&app, test::TestRequest::get().uri("/posts/first").to_request()).await;
    let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    assert_eq!(body, "PUBDET first");
    let res =
        test::call_service(&app, test::TestRequest::get().uri("/posts/nope").to_request()).await;
    assert_eq!(res.status().as_u16(), 404);

    // Public JSON API.
    let res = test::call_service(&app, test::TestRequest::get().uri("/api/posts").to_request()).await;
    assert_eq!(res.status().as_u16(), 200);
    let json: serde_json::Value = test::read_body_json(res).await;
    assert_eq!(json["total"], 1);
    assert_eq!(json["rows"][0]["slug"], "first");
    let res = test::call_service(
        &app,
        test::TestRequest::get().uri("/api/posts/first").to_request(),
    )
    .await;
    let json: serde_json::Value = test::read_body_json(res).await;
    assert_eq!(json["title"], "First!");

    // Delete: viewer forbidden, editor succeeds, second delete is 404.
    let res = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri(&location)
            .cookie(viewer.clone())
            .to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 401);
    let res = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri(&location)
            .cookie(editor.clone())
            .to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 200);
    let res = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri(&location)
            .cookie(editor.clone())
            .to_request(),
    )
    .await;
    assert_eq!(res.status().as_u16(), 404);

    assert!(Post::fetch(&db, id).await.unwrap().is_none());
}

/// App routes registered before the generated ones win on path conflicts —
/// the backend override mechanism.
#[actix_web::test]
async fn app_route_shadows_generated_route() {
    let db = SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    let app = test::init_service(
        App::new()
            .app_data(app_data(db))
            .configure(|cfg| {
                cfg.route(
                    "/posts",
                    web::get().to(|| async { HttpResponse::Ok().body("APP WINS") }),
                );
            })
            .configure(models::mount_all::<AppRole>),
    )
    .await;

    let res = test::call_service(&app, test::TestRequest::get().uri("/posts").to_request()).await;
    let body = String::from_utf8(test::read_body(res).await.to_vec()).unwrap();
    assert_eq!(body, "APP WINS");
}
