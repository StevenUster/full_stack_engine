//! The starter's own invariants, over the production stack
//! (`TestApp::new(starter::app())`: every route, module and middleware):
//! - the generated public catalog and API only ever expose `published`
//!   products (`public_scope`),
//! - the generated /admin/products honors the conventional permissions and
//!   the declarative field rules,
//! - orders: the custom customer flow keeps its ownership checks, managers
//!   fulfill/cancel through row actions.
//!
//! A refused request is a 404 here: the production stack renders 401/403 as
//! a 404 page, so a page's existence never leaks.

use full_stack_engine::testing::TestApp;
use starter::AppRole;
use starter::models::product::{Product, ProductStatus};
use starter::{count, insert};

async fn seed_product(app: &TestApp, slug: &str, status: ProductStatus) -> i64 {
    insert!(
        Product,
        &app.db,
        name = format!("Product {slug}"),
        slug = slug.to_string(),
        price = 9.99,
        status = status
    )
    .await
    .unwrap()
    .id
}

#[actix_web::test]
async fn public_pages_and_api_never_expose_unpublished_products() {
    let app = TestApp::new(starter::app()).await;
    seed_product(&app, "draft-product", ProductStatus::Draft).await;
    seed_product(&app, "archived-product", ProductStatus::Archived).await;
    seed_product(&app, "live-product", ProductStatus::Published).await;

    let res = app.get("/products").send().await;
    assert_eq!(res.status, 200);
    assert!(res.body.contains("live-product"));
    assert!(!res.body.contains("draft-product"));
    assert!(!res.body.contains("archived-product"));
    // The price is formatted by `#[ui(format = currency)]`.
    assert!(res.body.contains("9.99"), "formatted price missing");

    let json = app.get("/api/products").send().await.json();
    let slugs: Vec<&str> = json["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["live-product"]);

    for uri in ["/products/draft-product", "/api/products/draft-product"] {
        assert_eq!(app.get(uri).send().await.status, 404, "{uri}");
    }
    assert_eq!(app.get("/products/live-product").send().await.status, 200);
}

#[actix_web::test]
async fn generated_admin_crud_honors_permissions_and_field_rules() {
    let app = TestApp::new(starter::app()).await;
    let manager = app.user("manager@test.dev", AppRole::Manager).await;
    let user = app.user("user@test.dev", AppRole::User).await;

    // Plain users lack products.read.
    assert_eq!(
        app.get("/admin/products")
            .as_user(&user)
            .send()
            .await
            .status,
        404
    );

    // Managers get the generated list (theme template, real render) with
    // the create button.
    let res = app.get("/admin/products").as_user(&manager).send().await;
    assert_eq!(res.status, 200);
    assert!(res.body.contains(r#"products/create">New</a>"#));

    // Declarative rules: a negative price is rejected, nothing is written.
    let res = app
        .post("/admin/products/create")
        .as_user(&manager)
        .form(&[("name", "Bad"), ("price", "-1"), ("status", "draft")])
        .send()
        .await;
    assert_eq!(res.status, 200, "the form re-renders");
    assert_eq!(count!(Product, &app.db, all).await.unwrap(), 0);

    // An empty slug is derived from the name (`slug_from`).
    let res = app
        .post("/admin/products/create")
        .as_user(&manager)
        .form(&[
            ("name", "Generated Product"),
            ("slug", ""),
            ("price", "19.99"),
            ("status", "published"),
        ])
        .send()
        .await;
    assert_eq!(res.status, 302);
    let id = res.created_id().unwrap();
    let product = Product::fetch(&app.db, id).await.unwrap().unwrap();
    assert_eq!(product.slug, "generated-product");

    // The edit page is editable and deletable for the manager.
    let res = app
        .get(&format!("/admin/products/{id}"))
        .as_user(&manager)
        .send()
        .await;
    assert!(
        res.body.contains(r#"<fieldset class="contents">"#),
        "{}",
        res.body
    );
    assert!(res.body.contains("data-fse-delete"));

    // A duplicate slug re-renders instead of failing.
    let res = app
        .post("/admin/products/create")
        .as_user(&manager)
        .form(&[
            ("name", "Copycat"),
            ("slug", "generated-product"),
            ("status", "draft"),
        ])
        .send()
        .await;
    assert_eq!(res.status, 200);
    assert_eq!(count!(Product, &app.db, all).await.unwrap(), 1);

    // Plain users can't delete; managers can.
    let uri = format!("/admin/products/{id}");
    assert_eq!(app.delete(&uri).as_user(&user).send().await.status, 404);
    assert_eq!(app.delete(&uri).as_user(&manager).send().await.status, 200);
    assert_eq!(count!(Product, &app.db, all).await.unwrap(), 0);
}

#[actix_web::test]
async fn orders_customer_flow_and_manager_actions() {
    let app = TestApp::new(starter::app()).await;
    seed_product(&app, "draft-thing", ProductStatus::Draft).await;
    seed_product(&app, "live-thing", ProductStatus::Published).await;
    let buyer = app.user("buyer@test.dev", AppRole::User).await;
    let other = app.user("other@test.dev", AppRole::User).await;
    let manager = app.user("manager@test.dev", AppRole::Manager).await;

    // Anonymous order attempts bounce to login.
    let res = app
        .post("/products/live-thing/order")
        .form(&[("quantity", "1")])
        .send()
        .await;
    assert_eq!(res.location(), Some("/login"));

    // Draft products can't be ordered; published ones can.
    let res = app
        .post("/products/draft-thing/order")
        .as_user(&buyer)
        .form(&[("quantity", "1")])
        .send()
        .await;
    assert_eq!(res.status, 404);
    let res = app
        .post("/products/live-thing/order")
        .as_user(&buyer)
        .form(&[("quantity", "2")])
        .send()
        .await;
    assert_eq!(res.location(), Some("/my-orders"));
    let res = app.get("/my-orders").as_user(&buyer).send().await;
    assert!(res.body.contains("live-thing"));
    // Someone else's list doesn't show it.
    let res = app.get("/my-orders").as_user(&other).send().await;
    assert!(!res.body.contains("live-thing"));

    // Managers see the order by product and customer name, with the
    // pending-only actions.
    let res = app.get("/admin/orders").as_user(&manager).send().await;
    assert_eq!(res.status, 200);
    assert!(res.body.contains("Product live-thing"));
    assert!(res.body.contains("buyer@test.dev"));
    let order: i64 = sqlx::query_scalar("SELECT id FROM orders")
        .fetch_one(&app.db)
        .await
        .unwrap();
    // (Tera escapes `/` as `&#x2F;` in attributes, so match the tail.)
    assert!(
        res.body.contains(&format!("{order}/actions/fulfill")),
        "{}",
        res.body
    );

    let res = app
        .post(&format!("/admin/orders/{order}/actions/fulfill"))
        .as_user(&manager)
        .send()
        .await;
    assert_eq!(res.status, 303);
    // Fulfilled: no more actions, and the customer can't cancel it.
    let res = app
        .post(&format!("/admin/orders/{order}/actions/cancel"))
        .as_user(&manager)
        .send()
        .await;
    assert_eq!(
        res.status, 404,
        "denied — the production stack answers 401/403 with a 404 page"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM orders")
        .fetch_one(&app.db)
        .await
        .unwrap();
    assert_eq!(status, "fulfilled");
}
