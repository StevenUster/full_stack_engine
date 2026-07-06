//! Shared setup for integration-style tests: a real (in-memory, fully
//! migrated) database, the real embedded templates, and small seeding
//! helpers. Each `tests/*.rs` file compiles as its own crate, so this module
//! is pulled in with `mod common;` rather than living in `src/`.
//!
//! Not every helper is used by every test file that includes this module;
//! each test binary is compiled separately, so allow unused items rather than
//! duplicating helpers per-file.
#![allow(dead_code)]

use actix_web::web;
use starter::tables::product::InsertProduct;
use starter::tables::user::InsertUser;
use starter::tera::Tera;
use starter::{AppData, Env, hash_password};

pub const JWT_SECRET: &str = "test-secret";

/// Loads the embedded dist into a real `Tera` for tests. Unlike the
/// framework's own boot-time loader (which logs and skips a broken template
/// so one bad page doesn't take the whole app down at runtime),
/// `full_stack_engine::testing::load_templates` fails loudly with the full
/// Tera error chain, so a broken template (bad syntax, a typo'd variable, an
/// fse-ssr escaping bug) fails `cargo test`/CI instead of only surfacing as
/// a request-time 500.
pub fn test_tera() -> Tera {
    full_stack_engine::testing::load_templates(&starter::DIST_DIR).expect("broken template(s) found")
}

pub async fn test_app_data() -> web::Data<AppData> {
    // One connection only: every `sqlite::memory:` connection is its own DB.
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();

    web::Data::new(AppData {
        tera: test_tera(),
        db: pool,
        env: Env::Prod,
        domain: "localhost".to_string(),
        protocol: "http".to_string(),
        jwt_secret: JWT_SECRET.to_string(),
        smtp_from: String::new(),
        email_verification_enabled: false,
        // Same locale injection as production (`lib.rs`), since templates
        // reference `t.*`.
        context_injector: Some(std::sync::Arc::new(Box::new(|_req, value| {
            full_stack_engine::i18n::inject_locale_context(value, &starter::LOCALES_DIR, "en");
        }))),
    })
}

pub async fn seed_user(data: &web::Data<AppData>, email: &str, password: &str, role: &str) -> i64 {
    let hash = hash_password(password).unwrap();
    let user = InsertUser {
        role: <starter::AppRole as starter::Role>::from_role_str(role),
        ..InsertUser::new(email.into(), hash)
    }
    .insert(&data.db)
    .await
    .unwrap();
    user.id
}

/// Inserts a product with the given slug/status and returns its id.
pub async fn seed_product(data: &web::Data<AppData>, slug: &str, status: &str) -> i64 {
    let product = InsertProduct {
        price: 9.99,
        status: status.parse().unwrap(),
        ..InsertProduct::new(format!("Product {slug}"), slug.into())
    }
    .insert(&data.db)
    .await
    .unwrap();
    product.id
}

#[macro_export]
macro_rules! login_cookie {
    ($app:expr, $email:expr, $password:expr) => {{
        let req = actix_web::test::TestRequest::post()
            .uri("/login")
            .set_form([("email", $email), ("password", $password)])
            .to_request();
        let res = actix_web::test::call_service($app, req).await;
        assert_eq!(
            res.status(),
            actix_web::http::StatusCode::SEE_OTHER,
            "login should succeed"
        );
        res.response()
            .cookies()
            .find(|c| c.name() == "token")
            .expect("login sets the token cookie")
            .into_owned()
    }};
}
