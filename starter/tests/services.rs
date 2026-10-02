//! App-level smoke tests of the framework-provided flows wired into the
//! starter (the flows themselves are exhaustively tested in the framework):
//! the auth module runs with the starter's roles, and registered users get
//! the "user" role with no admin access.

use full_stack_engine::testing::{TEST_PASSWORD, TestApp};
use starter::AppRole;

#[actix_web::test]
async fn register_login_and_role_gates_work_end_to_end() {
    let app = TestApp::new(starter::app()).await;
    let admin = app.user("admin@test.dev", AppRole::Admin).await;

    // Register through the auth module.
    let res = app
        .post("/register")
        .form(&[
            ("first_name", "New"),
            ("last_name", "User"),
            ("email", "new@test.dev"),
            ("password", "password123"),
            ("repeat_password", "password123"),
        ])
        .send()
        .await;
    assert_eq!(res.status, 303);

    // The fresh account logs in through the real form...
    let res = app
        .post("/login")
        .form(&[("email", "new@test.dev"), ("password", "password123")])
        .send()
        .await;
    assert_eq!(res.status, 303, "login should succeed");
    let cookie = res
        .headers
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));

    // ...and TestApp users log in with TEST_PASSWORD.
    let res = app
        .post("/login")
        .form(&[("email", "admin@test.dev"), ("password", TEST_PASSWORD)])
        .send()
        .await;
    assert_eq!(res.status, 303);

    // Self-registered accounts have no user administration access; admins do.
    let newcomer = app.user("new@test.dev", AppRole::User).await;
    assert_eq!(
        app.get("/users").as_user(&newcomer).send().await.status,
        404
    );
    assert_eq!(app.get("/users").as_user(&admin).send().await.status, 200);
}
