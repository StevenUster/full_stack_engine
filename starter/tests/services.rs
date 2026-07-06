//! Integration-style tests: real handlers, a real (in-memory, fully migrated)
//! database and the real embedded templates. Routes are registered without
//! their rate limiters so tests never trip a 429.

mod common;

use actix_web::http::StatusCode;
use actix_web::{App, test, web};
use common::{seed_user, test_app_data};
use starter::services::{login, register, reset_password, users};

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

/// The register endpoint takes a multipart form; build one by hand.
fn multipart_register(email: &str) -> (String, Vec<u8>) {
    use std::fmt::Write as _;

    let boundary = "test-boundary-7d1a";
    let mut body = String::new();
    for (name, value) in [
        ("first_name", "Test"),
        ("last_name", "Tester"),
        ("email", email),
        ("password", "longpassword1"),
        ("repeat_password", "longpassword1"),
    ] {
        // Writing to a String is infallible.
        let _ = write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
        );
    }
    let _ = write!(body, "--{boundary}--\r\n");
    (
        format!("multipart/form-data; boundary={boundary}"),
        body.into_bytes(),
    )
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
async fn duplicate_registration_is_indistinguishable_from_success() {
    let data = test_app_data().await;
    let app = test_service!(data);

    let (content_type, body) = multipart_register("dup@test.io");
    let register = || {
        test::TestRequest::post()
            .uri("/register")
            .insert_header(("content-type", content_type.clone()))
            .set_payload(body.clone())
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
    // `manager` holds `users.read`/`users.write`, so it can reach every route
    // below — but must still be refused anything admin-adjacent.
    seed_user(&data, "mgr@test.io", "correct-password", "manager").await;
    let victim = seed_user(&data, "victim@test.io", "correct-password", "user").await;
    let admin = seed_user(&data, "root@test.io", "correct-password", "admin").await;
    let app = test_service!(data);

    let cookie = login_cookie!(&app, "mgr@test.io", "correct-password");

    // A manager can list users...
    let req = test::TestRequest::get()
        .uri("/users")
        .cookie(cookie.clone())
        .to_request();
    assert_eq!(test::call_service(&app, req).await.status(), StatusCode::OK);

    // ...but promoting anyone to admin is refused...
    let req = test::TestRequest::post()
        .uri(&format!("/users/{victim}"))
        .cookie(cookie.clone())
        .set_form([("role", "admin")])
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::UNAUTHORIZED
    );

    // ...and touching an existing admin account is refused too.
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
    let res = test::call_service(&app, req).await;
    assert!(res.status().is_client_error() || res.status().is_server_error());
    let role: String = sqlx::query_scalar("SELECT role FROM users WHERE id = ?")
        .bind(victim)
        .fetch_one(&data.db)
        .await
        .unwrap();
    assert_eq!(role, "user");

    // An admin, on the other hand, can change roles — and the change revokes
    // the target's sessions.
    let admin_cookie = login_cookie!(&app, "root@test.io", "correct-password");
    let req = test::TestRequest::post()
        .uri(&format!("/users/{victim}"))
        .cookie(admin_cookie)
        .set_form([("role", "manager")])
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::FOUND
    );

    let (role, valid_after): (String, i64) =
        sqlx::query_as("SELECT role, sessions_valid_after FROM users WHERE id = ?")
            .bind(victim)
            .fetch_one(&data.db)
            .await
            .unwrap();
    assert_eq!(role, "manager");
    assert!(valid_after > 0);
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
         reset_token_expires_at = datetime('now', '-1 hour') WHERE id = ?",
    )
    .bind(user)
    .execute(&data.db)
    .await
    .unwrap();
    let res = test::call_service(&app, reset("expired-tok")).await;
    // Redirected back to the form with an error, not to the success path.
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert!(
        res.headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("error=invalid_token")
    );
    let token: Option<String> = sqlx::query_scalar("SELECT reset_token FROM users WHERE id = ?")
        .bind(user)
        .fetch_one(&data.db)
        .await
        .unwrap();
    assert_eq!(token.as_deref(), Some("expired-tok"));

    // Valid token: consumed, password changed, outstanding sessions revoked.
    sqlx::query(
        "UPDATE users SET reset_token = 'valid-tok', \
         reset_token_expires_at = datetime('now', '+1 hour') WHERE id = ?",
    )
    .bind(user)
    .execute(&data.db)
    .await
    .unwrap();
    let res = test::call_service(&app, reset("valid-tok")).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.headers().get("location").unwrap(), "/logout");

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
