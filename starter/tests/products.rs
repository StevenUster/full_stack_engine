//! Integration tests for the product catalog: public endpoints must only
//! ever expose `published` products, and product-manager writes require the
//! `products.write` permission.

mod common;

use actix_web::http::StatusCode;
use actix_web::{App, test, web};
use common::{seed_product, seed_user, test_app_data};
use starter::count;
use starter::services::{api, login, orders, products};
use starter::tables::product::Product;

macro_rules! test_service {
    ($data:expr) => {
        test::init_service(
            App::new()
                .app_data($data.clone())
                .route("/login", web::post().to(login::post))
                .service(api::get_products)
                .service(api::get_product_detail)
                .service(products::get_public_products)
                .service(products::get_public_product_detail)
                .service(products::get_products)
                .service(products::post_product_create)
                .service(products::get_product)
                .service(products::post_product)
                .service(products::delete_product)
                .service(orders::post_place_order)
                .service(orders::get_my_orders),
        )
        .await
    };
}

#[actix_web::test]
async fn public_endpoints_never_expose_unpublished_products() {
    let data = test_app_data().await;
    seed_product(&data, "draft-product", "draft").await;
    seed_product(&data, "archived-product", "archived").await;
    seed_product(&data, "live-product", "published").await;
    let app = test_service!(data);

    // Astro-rendered public catalog.
    let req = test::TestRequest::get().uri("/products").to_request();
    let body = test::call_and_read_body(&app, req).await;
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("live-product") || body.contains("Product live-product"));
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
    assert!(slugs.contains(&"live-product"));
    assert!(!slugs.contains(&"draft-product"));
    assert!(!slugs.contains(&"archived-product"));

    // Direct detail lookup of a non-public product is a 404, not a leak.
    let req = test::TestRequest::get()
        .uri("/products/draft-product")
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[actix_web::test]
async fn only_products_write_permission_can_manage_the_catalog() {
    let data = test_app_data().await;
    seed_user(&data, "user@test.io", "correct-password", "user").await;
    seed_user(&data, "mgr@test.io", "correct-password", "manager").await;
    let app = test_service!(data);

    // A plain `user` role has neither `products.read` nor `products.write`.
    let cookie = login_cookie!(&app, "user@test.io", "correct-password");
    let req = test::TestRequest::post()
        .uri("/product-manager/create")
        .cookie(cookie)
        .set_form([("name", "Widget"), ("price", "9.99")])
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::UNAUTHORIZED
    );

    // `manager` has `products.write` and can create one.
    let cookie = login_cookie!(&app, "mgr@test.io", "correct-password");
    let req = test::TestRequest::post()
        .uri("/product-manager/create")
        .cookie(cookie)
        .set_form([("name", "Widget"), ("price", "9.99")])
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::FOUND
    );

    let count = count!(Product, &data.db, name == "Widget").await.unwrap();
    assert_eq!(count, 1);
}

#[actix_web::test]
async fn placing_an_order_requires_a_published_product_and_login() {
    let data = test_app_data().await;
    seed_product(&data, "draft-product", "draft").await;
    seed_product(&data, "live-product", "published").await;
    seed_user(&data, "user@test.io", "correct-password", "user").await;
    let app = test_service!(data);

    // Anonymous order attempts never reach the handler: the `AuthUser`
    // extractor redirects straight to `/login`.
    let req = test::TestRequest::post()
        .uri("/products/live-product/order")
        .set_form([("quantity", "1")])
        .to_request();
    let res = test::call_service(&app, req).await;
    assert_eq!(res.status(), StatusCode::FOUND);
    assert_eq!(res.headers().get("location").unwrap(), "/login");

    let cookie = login_cookie!(&app, "user@test.io", "correct-password");

    // Ordering a draft (unpublished) product 404s.
    let req = test::TestRequest::post()
        .uri("/products/draft-product/order")
        .cookie(cookie.clone())
        .set_form([("quantity", "1")])
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::NOT_FOUND
    );

    // Ordering the published product succeeds and shows up in "my orders".
    let req = test::TestRequest::post()
        .uri("/products/live-product/order")
        .cookie(cookie.clone())
        .set_form([("quantity", "2")])
        .to_request();
    assert_eq!(
        test::call_service(&app, req).await.status(),
        StatusCode::FOUND
    );

    let req = test::TestRequest::get()
        .uri("/my-orders")
        .cookie(cookie)
        .to_request();
    let body = test::call_and_read_body(&app, req).await;
    let body = String::from_utf8_lossy(&body);
    assert!(body.contains("live-product") || body.contains("Product live-product"));
}
