//! End-to-end tests of the built-in auth module: registration (validation,
//! duplicate handling, optional email verification), login (timing-safe,
//! role gate, cookie), logout, and the full password-reset lifecycle —
//! driven over real HTTP against the build.rs database (the `users` table is
//! defined once in models_http.rs).

use actix_web::cookie::Cookie;
use actix_web::http::header::{LOCATION, SET_COOKIE};
use actix_web::{App, test, web};
use full_stack_engine::prelude::tera;
use full_stack_engine::{AppData, Env, auth_module, define_roles};
use sqlx::SqlitePool;

define_roles! {
    (Admin, "admin", ["all"]),
    (User, "user", []),
    (None, "none", ["none"]),
}

const SECRET: &str = "0123456789abcdef0123456789abcdef";

/// A fresh IP per request: the module's per-IP rate limiters are part of the
/// mounted routes, and these tests intentionally hammer auth endpoints.
fn next_peer() -> std::net::SocketAddr {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(1);
    let n = N.fetch_add(1, Ordering::Relaxed);
    format!("192.0.{}.{}:9999", n / 200, n % 200 + 1).parse().unwrap()
}

fn test_tera() -> tera::Tera {
    let mut t = tera::Tera::default();
    t.autoescape_on(vec![""]);
    for name in ["login", "register", "register-success", "forgot-password"] {
        t.add_raw_template(
            name,
            &format!(
                "{name} error={{{{ error | default(value='') }}}} success={{{{ success | default(value='') }}}}"
            ),
        )
        .unwrap();
    }
    t.add_raw_template(
        "reset-password",
        "reset-password token={{ token }} error={{ error | default(value='') }}",
    )
    .unwrap();
    t.add_raw_template("emails/verify", "VERIFY {{ verify_url }}").unwrap();
    t.add_raw_template("emails/password-reset", "RESET {{ reset_url }}")
        .unwrap();
    t
}

fn app_data(db: SqlitePool, verification: bool) -> web::Data<AppData> {
    web::Data::new(AppData {
        tera: test_tera(),
        db,
        env: Env::Prod,
        domain: "test.dev".into(),
        protocol: "https".into(),
        jwt_secret: SECRET.to_string(),
        smtp_from: String::new(),
        email_verification_enabled: verification,
        context_injector: None,
        locales: std::collections::HashMap::new(),
        locale_selector: full_stack_engine::i18n::LocaleSelector::default(),
    })
}

macro_rules! auth_app {
    ($db:expr, $verification:expr) => {{
        let module = auth_module::module::<AppRole>();
        test::init_service(
            App::new()
                .app_data(app_data($db.clone(), $verification))
                .configure(module.routes.expect("auth module has routes")),
        )
        .await
    }};
}

macro_rules! post_form {
    ($app:expr, $uri:expr, $pairs:expr) => {
        test::call_service(
            $app,
            test::TestRequest::post()
                .uri($uri)
                .peer_addr(next_peer())
                .set_form($pairs)
                .to_request(),
        )
        .await
    };
}

async fn body_string(res: actix_web::dev::ServiceResponse) -> String {
    String::from_utf8(test::read_body(res).await.to_vec()).unwrap()
}

fn location_of(res: &actix_web::dev::ServiceResponse) -> String {
    res.headers()
        .get(LOCATION)
        .expect("redirect")
        .to_str()
        .unwrap()
        .to_string()
}

#[actix_web::test]
async fn register_login_logout_and_password_reset() {
    let db = SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    sqlx::query("DELETE FROM users WHERE email LIKE 'flow-%'")
        .execute(&db)
        .await
        .unwrap();
    let app = auth_app!(db, false);

    // Validation: mismatched passwords re-render with the error.
    let res = post_form!(
        &app,
        "/register",
        [
            ("first_name", "Flo"),
            ("last_name", "Flow"),
            ("email", "flow-a@test.dev"),
            ("password", "longenough"),
            ("repeat_password", "different"),
        ]
    );
    assert_eq!(body_string(res).await, "register error=passwords_mismatch success=");

    // Valid registration (verification disabled) goes straight to login.
    let res = post_form!(
        &app,
        "/register",
        [
            ("first_name", "Flo"),
            ("last_name", "Flow"),
            ("email", "Flow-A@test.dev"),
            ("password", "longenough"),
            ("repeat_password", "longenough"),
        ]
    );
    assert_eq!(res.status().as_u16(), 303);
    assert_eq!(location_of(&res), "/login");

    // Stored lowercased, hashed, with the self-registration role.
    let (role, password, is_verified): (String, String, bool) = sqlx::query_as(
        "SELECT role, password, is_verified FROM users WHERE email = 'flow-a@test.dev'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(role, "user");
    assert!(password.starts_with("$argon2"));
    assert!(is_verified);

    // Registering the same email again responds exactly like success.
    let res = post_form!(
        &app,
        "/register",
        [
            ("first_name", "Eve"),
            ("last_name", "Sdropper"),
            ("email", "flow-a@test.dev"),
            ("password", "longenough"),
            ("repeat_password", "longenough"),
        ]
    );
    assert_eq!(res.status().as_u16(), 303);
    assert_eq!(location_of(&res), "/login");

    // Wrong password: same page, no cookie.
    let res = post_form!(
        &app,
        "/login",
        [("email", "flow-a@test.dev"), ("password", "wrong-password")]
    );
    assert_eq!(body_string(res).await, "login error=invalid_credentials success=");

    // Correct password: redirect home with an HttpOnly session cookie.
    let res = post_form!(
        &app,
        "/login",
        [("email", "flow-a@test.dev"), ("password", "longenough")]
    );
    assert_eq!(res.status().as_u16(), 303);
    assert_eq!(location_of(&res), "/");
    let set_cookie = res.headers().get(SET_COOKIE).unwrap().to_str().unwrap();
    assert!(set_cookie.starts_with("token="), "{set_cookie}");
    assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
    assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");

    // Logout clears the cookie.
    let res = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/logout")
            .peer_addr(next_peer())
            .cookie(Cookie::new("token", "whatever"))
            .to_request(),
    )
    .await;
    assert_eq!(location_of(&res), "/login");
    let cleared = res.headers().get(SET_COOKIE).unwrap().to_str().unwrap();
    assert!(cleared.starts_with("token=;"), "{cleared}");

    // Forgot password: unknown address gets the identical success page.
    let res = post_form!(&app, "/forgot-password", [("email", "flow-nobody@test.dev")]);
    assert_eq!(
        body_string(res).await,
        "forgot-password error= success=password_reset_sent"
    );

    // Known address: token lands in the database.
    let app2 = auth_app!(db, false); // fresh limiter (1/hour on this route)
    let res = post_form!(&app2, "/forgot-password", [("email", "flow-a@test.dev")]);
    assert_eq!(
        body_string(res).await,
        "forgot-password error= success=password_reset_sent"
    );
    let (token,): (String,) =
        sqlx::query_as("SELECT reset_token FROM users WHERE email = 'flow-a@test.dev'")
            .fetch_one(&db)
            .await
            .unwrap();

    // Reset form needs its token; a short password bounces back.
    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/reset-password?token={token}"))
            .peer_addr(next_peer())
            .to_request(),
    )
    .await;
    assert_eq!(
        body_string(res).await,
        format!("reset-password token={token} error=")
    );
    let res = post_form!(
        &app,
        "/reset-password",
        [("token", token.as_str()), ("password", "short"), ("repeat_password", "short")]
    );
    assert_eq!(res.status().as_u16(), 303);
    assert!(location_of(&res).contains("password_too_short"));

    // A bogus token never consumes anything.
    let res = post_form!(
        &app,
        "/reset-password",
        [("token", "bogus"), ("password", "newpassword"), ("repeat_password", "newpassword")]
    );
    assert!(location_of(&res).contains("invalid_token"));

    // The real token sets the new password, clears itself, and bumps
    // sessions_valid_after so old JWTs die.
    let res = post_form!(
        &app,
        "/reset-password",
        [("token", token.as_str()), ("password", "newpassword"), ("repeat_password", "newpassword")]
    );
    assert_eq!(location_of(&res), "/logout");
    let (reset_token, valid_after): (Option<String>, i64) = sqlx::query_as(
        "SELECT reset_token, sessions_valid_after FROM users WHERE email = 'flow-a@test.dev'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(reset_token.is_none());
    assert!(valid_after > 0);

    // Old password dead, new one works.
    let res = post_form!(
        &app,
        "/login",
        [("email", "flow-a@test.dev"), ("password", "longenough")]
    );
    assert_eq!(body_string(res).await, "login error=invalid_credentials success=");
    let res = post_form!(
        &app,
        "/login",
        [("email", "flow-a@test.dev"), ("password", "newpassword")]
    );
    assert_eq!(res.status().as_u16(), 303);
}

#[actix_web::test]
async fn email_verification_gate() {
    let db = SqlitePool::connect(env!("DATABASE_URL")).await.unwrap();
    sqlx::query("DELETE FROM users WHERE email LIKE 'verify-%'")
        .execute(&db)
        .await
        .unwrap();
    let app = auth_app!(db, true);

    // Registration with verification enabled: register-success + a token.
    let res = post_form!(
        &app,
        "/register",
        [
            ("first_name", "Vera"),
            ("last_name", "Fied"),
            ("email", "verify-a@test.dev"),
            ("password", "longenough"),
            ("repeat_password", "longenough"),
        ]
    );
    assert_eq!(location_of(&res), "/register-success");
    let (token, is_verified): (String, bool) = sqlx::query_as(
        "SELECT verification_token, is_verified FROM users WHERE email = 'verify-a@test.dev'",
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert!(!is_verified);

    // Unverified accounts can't log in.
    let res = post_form!(
        &app,
        "/login",
        [("email", "verify-a@test.dev"), ("password", "longenough")]
    );
    assert_eq!(body_string(res).await, "login error=confirm_email success=");

    // A wrong token verifies nothing; the real one flips the account.
    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/verify-email?token=bogus")
            .peer_addr(next_peer())
            .to_request(),
    )
    .await;
    assert_eq!(body_string(res).await, "login error=invalid_token success=");

    let res = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/verify-email?token={token}"))
            .peer_addr(next_peer())
            .to_request(),
    )
    .await;
    assert_eq!(body_string(res).await, "login error= success=email_confirmed");

    let res = post_form!(
        &app,
        "/login",
        [("email", "verify-a@test.dev"), ("password", "longenough")]
    );
    assert_eq!(res.status().as_u16(), 303);
    assert_eq!(location_of(&res), "/");
}
