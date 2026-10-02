//! Tests run the production stack (`TestApp::new(app())`) over an in-memory,
//! migrated database. A refused request is a 404: the stack never reveals
//! that a page exists to someone who may not see it.

use __CRATE__::AppRole;
use full_stack_engine::testing::TestApp;

#[actix_web::test]
async fn notes_belong_to_their_author() {
    let app = TestApp::new(__CRATE__::app()).await;
    let alice = app.user("alice@test.dev", AppRole::User).await;
    let bob = app.user("bob@test.dev", AppRole::User).await;

    let res = app
        .post("/admin/notes/create")
        .as_user(&alice)
        .form(&[("title", "Groceries"), ("body", "Milk")])
        .send()
        .await;
    assert_eq!(res.status, 302);
    let note = res.created_id().unwrap();

    let res = app.get("/admin/notes").as_user(&alice).send().await;
    assert!(res.body.contains("Groceries"));

    // Bob can't see, change or delete Alice's note.
    let res = app.get("/admin/notes").as_user(&bob).send().await;
    assert!(!res.body.contains("Groceries"));
    let uri = format!("/admin/notes/{note}");
    assert_eq!(app.get(&uri).as_user(&bob).send().await.status, 404);
    assert_eq!(app.delete(&uri).as_user(&bob).send().await.status, 404);
}
