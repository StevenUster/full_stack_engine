//! Exercises `include:` — Prisma-style eager relation loading via a real SQL
//! JOIN, still passed through a literal `sqlx::query!` so sqlx checks the
//! joined columns too.

use fse_orm::{find, find_one};
use tests_app::tables::event::InsertEvent;
use tests_app::tables::product::{InsertProduct, Product, ProductStatus};
use tests_app::tables::review::{InsertReview, Review};

async fn setup() -> (sqlx::SqlitePool, i64, i64) {
    let copy = std::env::temp_dir().join(format!(
        "fse-orm-relations-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
    ));
    std::fs::copy("db/test.db", &copy).expect("template db from build.rs");
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(&copy).foreign_keys(true);
    let db = sqlx::SqlitePool::connect_with(options).await.unwrap();

    let event = InsertEvent::new("Fair".into()).insert(&db).await.unwrap();
    let product = InsertProduct {
        status: ProductStatus::Published,
        ..InsertProduct::new("mug".into(), "Coffee Mug".into(), event.id)
    }
    .insert(&db)
    .await
    .unwrap();

    InsertReview {
        product_id: Some(product.id),
        comment: Some("Great!".into()),
        ..InsertReview::new(5)
    }
    .insert(&db)
    .await
    .unwrap();
    // No product: exercises the LEFT JOIN "no match" branch directly (rather
    // than relying on ON DELETE SET NULL, which is exercised separately below).
    InsertReview::new(1).insert(&db).await.unwrap();

    (db, event.id, product.id)
}

#[tokio::test]
async fn inner_join_relation_loads_the_parent() {
    let (db, event_id, _) = setup().await;

    // Product::event is an INNER JOIN relation (event_id is NOT NULL).
    let product = find_one!(Product, &db, slug == "mug", include: [event]).await.unwrap().unwrap();
    let event = product.event.expect("INNER JOIN relation always resolves");
    assert_eq!(event.id, event_id);
    assert_eq!(event.name, "Fair");

    // find! (plural) with include: too, plus an ordinary filter alongside it.
    let products =
        find!(Product, &db, status == ProductStatus::Published, include: [event]).await.unwrap();
    assert_eq!(products.len(), 1);
    assert_eq!(products[0].event.as_ref().unwrap().name, "Fair");

    // Without include:, the relation field stays None — same SQL as before
    // this feature existed, no join.
    let bare = find_one!(Product, &db, slug == "mug").await.unwrap().unwrap();
    assert!(bare.event.is_none());
}

#[tokio::test]
async fn left_join_relation_is_none_when_the_parent_is_absent() {
    let (db, _, product_id) = setup().await;

    let with_product =
        find_one!(Review, &db, rating == 5, include: [product]).await.unwrap().unwrap();
    let product = with_product.product.expect("this review has a product");
    assert_eq!(product.id, product_id);
    assert_eq!(product.name, "Coffee Mug");
    // The joined struct's own enum/json columns still convert correctly.
    assert_eq!(product.status, ProductStatus::Published);

    let orphan = find_one!(Review, &db, rating == 1, include: [product]).await.unwrap().unwrap();
    assert!(orphan.product.is_none(), "no product_id -> LEFT JOIN finds no row -> None");

    // ON DELETE SET NULL: deleting the product nulls product_id on the
    // review, and a later include: still resolves cleanly to None.
    Product::delete(&db, product_id).await.unwrap();
    let after_delete =
        find_one!(Review, &db, rating == 5, include: [product]).await.unwrap().unwrap();
    assert!(after_delete.product.is_none());
    assert_eq!(after_delete.product_id, None);
}

#[tokio::test]
async fn multiple_reviews_include_independently() {
    let (db, _, _) = setup().await;

    let reviews = find!(Review, &db, all, order_by: id.asc(), include: [product]).await.unwrap();
    assert_eq!(reviews.len(), 2);
    assert!(reviews[0].product.is_some());
    assert!(reviews[1].product.is_none());
}
