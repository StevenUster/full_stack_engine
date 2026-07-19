//! App-level smoke tests of the framework-provided flows wired into the
//! starter (the flows themselves are exhaustively tested in the framework):
//! the auth module runs with the starter's roles, and registered users get
//! the "user" role with no admin access.

mod common;

use actix_web::http::StatusCode;
use actix_web::test;
use common::{next_peer, seed_user, test_app_data};

#[actix_web::test]
async fn register_login_and_role_gates_work_end_to_end() {
    let data = test_app_data().await;
    seed_user(&data, "admin@test.dev", "password123", "admin").await;
    let app = test_app!(data);

    // Register through the auth module (urlencoded form).
    let req = test::TestRequest::post()
        .uri("/register")
        .peer_addr(next_peer())
        .set_form([
            ("first_name", "New"),
            ("last_name", "User"),
            ("email", "new@test.dev"),
            ("password", "password123"),
            ("repeat_password", "password123"),
        ])
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);

    // The fresh account logs in and holds the self-registration role...
    let cookie = login_cookie!(&app, "new@test.dev", "password123");

    // ...which has no user administration access.
    let req = test::TestRequest::get()
        .uri("/users")
        .cookie(cookie.clone())
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Admin sees the users list (framework page, starter roles).
    let admin = login_cookie!(&app, "admin@test.dev", "password123");
    let req = test::TestRequest::get().uri("/users").cookie(admin).to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::OK);
}
