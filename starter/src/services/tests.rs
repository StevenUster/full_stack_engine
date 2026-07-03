//! Integration-style tests: real handlers, a real (in-memory, fully migrated)
//! database and the real embedded templates. Routes are registered without
//! their rate limiters so tests never trip a 429.

use super::{login, register, reset_password, users};
use crate::tera::Tera;
use crate::{AppData, Env, hash_password};
use actix_web::http::StatusCode;
use actix_web::{App, test, web};

const JWT_SECRET: &str = "test-secret";

/// Replicates the framework's template registration (`index.html` -> `index`,
/// `login/index.html` -> `login`, forced autoescape) for the embedded dist.
fn test_tera() -> Tera {
    fn add(tera: &mut Tera, dir: &crate::include_dir::Dir) {
        for file in dir.files() {
            let path = file.path().to_str().unwrap().replace('\\', "/");
            if let Some(stripped) = path.strip_suffix(".html") {
                let name = match stripped
                    .strip_suffix("/index")
                    .or_else(|| (stripped == "index").then_some("index"))
                {
                    Some(n) => n,
                    None => stripped,
                };
                if let Some(content) = file.contents_utf8() {
                    let _ = tera.add_raw_template(name, content);
                }
            }
        }
        for sub in dir.dirs() {
            add(tera, sub);
        }
    }

    let mut tera = Tera::default();
    tera.autoescape_on(vec![""]);
    add(&mut tera, &crate::DIST_DIR);
    tera
}

async fn test_app_data() -> web::Data<AppData> {
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
        // Same locale injection as production (`main.rs`), since templates
        // reference `t.*`.
        context_injector: Some(std::sync::Arc::new(Box::new(|_req, value| {
            full_stack_engine::i18n::inject_locale_context(value, &crate::LOCALES_DIR, "en");
        }))),
    })
}

async fn seed_user(data: &web::Data<AppData>, email: &str, password: &str, role: &str) -> i64 {
    let hash = hash_password(password).unwrap();
    sqlx::query("INSERT INTO users (email, password, role, is_verified) VALUES (?, ?, ?, 1)")
        .bind(email)
        .bind(hash)
        .bind(role)
        .execute(&data.db)
        .await
        .unwrap()
        .last_insert_rowid()
}

macro_rules! test_service {
    ($data:expr) => {
        test::init_service(
            App::new()
                .app_data($data.clone())
                .route("/login", web::post().to(login::post))
                .route("/register", web::post().to(register::post))
                .route("/reset-password", web::post().to(reset_password::post))
                .service(users::get)
                .service(users::post_user)
                .service(users::delete_user),
        )
        .await
    };
}

macro_rules! login_cookie {
    ($app:expr, $email:expr, $password:expr) => {{
        let req = test::TestRequest::post()
            .uri("/login")
            .set_form([("email", $email), ("password", $password)])
            .to_request();
        let res = test::call_service($app, req).await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER, "login should succeed");
        res.response()
            .cookies()
            .find(|c| c.name() == "token")
            .expect("login sets the token cookie")
            .into_owned()
    }};
}

#[actix_web::test]
async fn login_rejects_wrong_password_and_unknown_user_identically() {
    let data = test_app_data().await;
    seed_user(&data, "known@test.io", "correct-password", "user").await;
    let app = test_service!(data);

    for (email, password) in [
        ("known@test.io", "wrong-password"),
        ("unknown@test.io", "whatever-password"),
    ] {
        let req = test::TestRequest::post()
            .uri("/login")
            .set_form([("email", email), ("password", password)])
            .to_request();
        let res = test::call_service(&app, req).await;
        // Both cases re-render the login page instead of leaking which part
        // was wrong (no account enumeration).
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.response().cookies().all(|c| c.name() != "token"));
    }
}

#[actix_web::test]
async fn login_sets_hardened_session_cookie() {
    let data = test_app_data().await;
    seed_user(&data, "user@test.io", "correct-password", "user").await;
    let app = test_service!(data);

    let cookie = login_cookie!(&app, "user@test.io", "correct-password");
    assert_eq!(cookie.http_only(), Some(true));
    assert_eq!(
        cookie.same_site(),
        Some(actix_web::cookie::SameSite::Strict)
    );
    // Env::Prod in the test AppData -> Secure cookie.
    assert_eq!(cookie.secure(), Some(true));
}

#[actix_web::test]
async fn login_rejects_none_role_accounts() {
    let data = test_app_data().await;
    seed_user(&data, "locked@test.io", "correct-password", "none").await;
    let app = test_service!(data);

    let req = test::TestRequest::post()
        .uri("/login")
        .set_form([
            ("email", "locked@test.io"),
            ("password", "correct-password"),
        ])
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.response().cookies().all(|c| c.name() != "token"));
}

#[actix_web::test]
async fn duplicate_registration_is_indistinguishable_from_success() {
    let data = test_app_data().await;
    let app = test_service!(data);

    let register = || {
        test::TestRequest::post()
            .uri("/register")
            .set_form([
                ("email", "dup@test.io"),
                ("password", "longpassword1"),
                ("repeat_password", "longpassword1"),
            ])
            .to_request()
    };

    let first = test::call_service(&app, register()).await;
    assert_eq!(first.status(), StatusCode::SEE_OTHER);

    // Second registration with the same email: same redirect, no 500 from the
    // UNIQUE constraint, and still exactly one row.
    let second = test::call_service(&app, register()).await;
    assert_eq!(second.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        first.headers().get("location"),
        second.headers().get("location")
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE email = 'dup@test.io'")
        .fetch_one(&data.db)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[actix_web::test]
async fn users_write_permission_cannot_grant_or_touch_admin() {
    let data = test_app_data().await;
    seed_user(&data, "mgr@test.io", "correct-password", "manager").await;
    let victim = seed_user(&data, "victim@test.io", "correct-password", "user").await;
    let admin = seed_user(&data, "root@test.io", "correct-password", "admin").await;
    let app = test_service!(data);

    let cookie = login_cookie!(&app, "mgr@test.io", "correct-password");

    // Promoting anyone to admin is refused for non-admin callers.
    let req = test::TestRequest::post()
        .uri(&format!("/users/{victim}"))
        .cookie(cookie.clone())
        .set_form([("role", "admin")])
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Demoting or deleting an admin is refused too.
    let req = test::TestRequest::post()
        .uri(&format!("/users/{admin}"))
        .cookie(cookie.clone())
        .set_form([("role", "user")])
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let req = test::TestRequest::delete()
        .uri(&format!("/users/{admin}"))
        .cookie(cookie.clone())
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::UNAUTHORIZED
    );

    // Unknown role strings are rejected instead of silently stored.
    let req = test::TestRequest::post()
        .uri(&format!("/users/{victim}"))
        .cookie(cookie)
        .set_form([("role", "hacker")])
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::BAD_REQUEST
    );

    // Nothing changed in the database.
    let roles: Vec<String> = sqlx::query_scalar("SELECT role FROM users ORDER BY id")
        .fetch_all(&data.db)
        .await
        .unwrap();
    assert!(roles.contains(&"manager".to_string()));
    assert!(roles.contains(&"user".to_string()));
    assert!(roles.contains(&"admin".to_string()));
}

#[actix_web::test]
async fn role_change_revokes_existing_sessions() {
    let data = test_app_data().await;
    seed_user(&data, "root@test.io", "correct-password", "admin").await;
    let target = seed_user(&data, "mgr@test.io", "correct-password", "manager").await;
    let app = test_service!(data);

    // The manager logs in and can read /users...
    let manager_cookie = login_cookie!(&app, "mgr@test.io", "correct-password");
    let req = test::TestRequest::get()
        .uri("/users")
        .cookie(manager_cookie.clone())
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), StatusCode::OK);

    // ...but the moment an admin changes their role, the old JWT dies.
    let admin_cookie = login_cookie!(&app, "root@test.io", "correct-password");
    let req = test::TestRequest::post()
        .uri(&format!("/users/{target}"))
        .cookie(admin_cookie)
        .set_form([("role", "user")])
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::FOUND
    );

    let req = test::TestRequest::get()
        .uri("/users")
        .cookie(manager_cookie)
        .to_request();
    let res = test::call_service(&app, req).await;
    // The stale session is redirected to /login by the auth extractor.
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(res.headers().get("location").unwrap(), "/login");
}

#[actix_web::test]
async fn password_reset_enforces_expiry_and_revokes_sessions() {
    let data = test_app_data().await;
    let user = seed_user(&data, "user@test.io", "correct-password", "user").await;
    let app = test_service!(data);

    let reset = |token: &str| {
        test::TestRequest::post()
            .uri("/reset-password")
            .set_form([
                ("token", token),
                ("password", "brand-new-pass1"),
                ("repeat_password", "brand-new-pass1"),
            ])
            .to_request()
    };

    // Expired token: rejected, nothing consumed.
    sqlx::query(
        "UPDATE users SET reset_token = 'expired-tok', \
         reset_token_expires_at = strftime('%s','now') - 10 WHERE id = ?",
    )
    .bind(user)
    .execute(&data.db)
    .await
    .unwrap();
    let res = test::call_service(&app, reset("expired-tok")).await;
    assert_eq!(res.status(), StatusCode::OK); // re-rendered with an error
    let token: Option<String> = sqlx::query_scalar("SELECT reset_token FROM users WHERE id = ?")
        .bind(user)
        .fetch_one(&data.db)
        .await
        .unwrap();
    assert_eq!(token.as_deref(), Some("expired-tok"));

    // Valid token: consumed, password changed, outstanding sessions revoked.
    sqlx::query(
        "UPDATE users SET reset_token = 'valid-tok', \
         reset_token_expires_at = strftime('%s','now') + 3600 WHERE id = ?",
    )
    .bind(user)
    .execute(&data.db)
    .await
    .unwrap();
    let res = test::call_service(&app, reset("valid-tok")).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    let (token, valid_after): (Option<String>, i64) =
        sqlx::query_as("SELECT reset_token, sessions_valid_after FROM users WHERE id = ?")
            .bind(user)
            .fetch_one(&data.db)
            .await
            .unwrap();
    assert_eq!(token, None);
    assert!(valid_after > 0);

    // The new password works, the old one doesn't.
    let req = test::TestRequest::post()
        .uri("/login")
        .set_form([("email", "user@test.io"), ("password", "correct-password")])
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), StatusCode::OK);
    let _ = login_cookie!(&app, "user@test.io", "brand-new-pass1");
}
