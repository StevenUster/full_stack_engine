//! The starter's own invariants, over the full production route stack
//! (overrides > auth module > generated CRUD):
//! - the hand-written public catalog only ever exposes `published` products
//!   even though the admin CRUD is generated,
//! - the generated /admin/products endpoints honor the conventional
//!   permissions,
//! - the custom order flows keep their ownership checks.

mod common;

use actix_web::http::StatusCode;
use actix_web::test;
use common::{next_peer, seed_product, seed_user, test_app_data};
use starter::count;
use starter::models::product::Product;

#[actix_web::test]
async fn public_endpoints_never_expose_unpublished_products() {
    let data = test_app_data().await;
    seed_product(&data, "draft-product", "draft").await;
    seed_product(&data, "archived-product", "archived").await;
    seed_product(&data, "live-product", "published").await;
    let app = test_app!(data);

    // Astro-rendered public catalog (hand-written override route).
    let req = test::TestRequest::get().uri("/products").to_request();
    let body = test::call_and_read_body(&app, req).await;
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("live-product"));
    assert!(!body.contains("draft-product"));
    assert!(!body.contains("archived-product"));

    // Same rule for the public JSON API.
    let req = test::TestRequest::get().uri("/api/products").to_request();
    let json: starter::serde_json::Value = test::call_and_read_body_json(&app, req).await;
    let slugs: Vec<&str> = json["products"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, ["live-product"]);

    // Unpublished detail pages are 404s, on the page and the API.
    for uri in ["/products/draft-product", "/api/products/draft-product"] {
        let req = test::TestRequest::get().uri(uri).to_request();
        let res = test::call_service(&app, req).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{uri}");
    }
}

#[actix_web::test]
async fn generated_admin_crud_honors_permissions() {
    let data = test_app_data().await;
    seed_user(&data, "manager@test.dev", "password123", "manager").await;
    seed_user(&data, "user@test.dev", "password123", "user").await;
    let app = test_app!(data);

    let manager = login_cookie!(&app, "manager@test.dev", "password123");
    let user = login_cookie!(&app, "user@test.dev", "password123");

    // Plain users lack products.read.
    let req = test::TestRequest::get()
        .uri("/admin/products")
        .cookie(user.clone())
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // Managers get the generated list page (theme template, real render).
    let req = test::TestRequest::get()
        .uri("/admin/products")
        .cookie(manager.clone())
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::OK);

    // Create through the generated form endpoint...
    let req = test::TestRequest::post()
        .uri("/admin/products/create")
        .cookie(manager.clone())
        .set_form([
            ("name", "Generated Product"),
            ("slug", "generated-product"),
            ("description", ""),
            ("price", "19.99"),
            ("status", "published"),
        ])
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::FOUND);
    let location = res
        .headers()
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let id: i64 = location.rsplit('/').next().unwrap().parse().unwrap();
    assert_eq!(count!(Product, &data.db, slug == "generated-product").await.unwrap(), 1);

    // ...validation errors re-render instead of writing (duplicate slug)...
    let req = test::TestRequest::post()
        .uri("/admin/products/create")
        .cookie(manager.clone())
        .set_form([
            ("name", "Copycat"),
            ("slug", "generated-product"),
            ("description", ""),
            ("price", "1"),
            ("status", "draft"),
        ])
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::OK); // form re-render, no redirect
    assert_eq!(count!(Product, &data.db, all).await.unwrap(), 1);

    // ...and plain users can't delete.
    let req = test::TestRequest::delete()
        .uri(&format!("/admin/products/{id}"))
        .cookie(user)
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let req = test::TestRequest::delete()
        .uri(&format!("/admin/products/{id}"))
        .cookie(manager)
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(count!(Product, &data.db, all).await.unwrap(), 0);
}

#[actix_web::test]
async fn placing_an_order_requires_a_published_product_and_login() {
    let data = test_app_data().await;
    seed_product(&data, "draft-thing", "draft").await;
    seed_product(&data, "live-thing", "published").await;
    seed_user(&data, "buyer@test.dev", "password123", "user").await;
    let app = test_app!(data);

    // Anonymous order attempts bounce to login.
    let req = test::TestRequest::post()
        .uri("/products/live-thing/order")
        .peer_addr(next_peer())
        .set_form([("quantity", "1")])
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::FOUND);

    let buyer = login_cookie!(&app, "buyer@test.dev", "password123");

    // Draft products cannot be ordered even by a signed-in user.
    let req = test::TestRequest::post()
        .uri("/products/draft-thing/order")
        .cookie(buyer.clone())
        .set_form([("quantity", "1")])
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    // Published ones can; the order shows up under /my-orders.
    let req = test::TestRequest::post()
        .uri("/products/live-thing/order")
        .cookie(buyer.clone())
        .set_form([("quantity", "2")])
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::FOUND);

    let req = test::TestRequest::get()
        .uri("/my-orders")
        .cookie(buyer)
        .to_request();
    let body = test::call_and_read_body(&app, req).await;
    assert!(String::from_utf8_lossy(&body).contains("live-thing"));
}
