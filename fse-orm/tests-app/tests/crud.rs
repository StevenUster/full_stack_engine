//! Runs the derive-generated CRUD against a real SQLite database (a fresh
//! copy of the build-script-created db/test.db per test).

use std::sync::atomic::{AtomicU64, Ordering};

use fse_orm::insert;
use tests_app::tables::event::Event;
use tests_app::tables::product::{Dimensions, Product, ProductStatus};

static NEXT_DB: AtomicU64 = AtomicU64::new(0);

async fn setup() -> sqlx::SqlitePool {
    let copy = std::env::temp_dir().join(format!(
        "fse-orm-crud-{}-{}.db",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy("db/test.db", &copy).expect("template db from build.rs");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&copy)
        .foreign_keys(true);
    sqlx::SqlitePool::connect_with(options).await.unwrap()
}

#[tokio::test]
async fn full_crud_roundtrip() {
    let db = setup().await;

    let event = insert!(Event, &db, name = "Spring Fair".to_string()).await.unwrap();
    assert!(event.id > 0);
    assert_eq!(event.name, "Spring Fair");

    // Columns left out (price, active, created_at) get filled by their own
    // `#[orm(default = ...)]` DDL default — never reproduced in Rust.
    let product = insert!(
        Product,
        &db,
        slug = "t-shirt".to_string(),
        name = "T-Shirt".to_string(),
        event_id = event.id,
        status = ProductStatus::Published,
        description = Some("A nice shirt".to_string()),
        dimensions = Some(Dimensions { width_cm: 30.0, height_cm: 40.0 })
    )
    .await
    .unwrap();

    assert!(product.id > 0);
    assert_eq!(product.price, 0.0, "default = 0.0");
    assert!(product.active, "default = true");
    assert_eq!(product.status, ProductStatus::Published, "explicit override");
    let age = chrono::Utc::now().naive_utc() - product.created_at;
    assert!(age.num_seconds().abs() < 10, "default = now");
    assert_eq!(
        product.dimensions,
        Some(Dimensions { width_cm: 30.0, height_cm: 40.0 }),
        "json column roundtrips through serde"
    );

    // Reads: by pk, by unique column, all, count.
    let fetched = Product::fetch(&db, product.id).await.unwrap().unwrap();
    assert_eq!(fetched.slug, "t-shirt");
    assert_eq!(fetched.dimensions, product.dimensions);

    let by_slug = Product::fetch_by_slug(&db, "t-shirt").await.unwrap().unwrap();
    assert_eq!(by_slug.id, product.id);
    assert!(Product::fetch_by_slug(&db, "missing").await.unwrap().is_none());

    assert_eq!(Product::fetch_all(&db).await.unwrap().len(), 1);
    assert_eq!(Product::count(&db).await.unwrap(), 1);

    // Full-row update, including enum and json changes.
    let mut updated = fetched;
    updated.name = "Premium T-Shirt".into();
    updated.status = ProductStatus::Archived;
    updated.description = None;
    updated.dimensions = Some(Dimensions { width_cm: 31.5, height_cm: 41.0 });
    updated.update(&db).await.unwrap();

    let reread = Product::fetch(&db, product.id).await.unwrap().unwrap();
    assert_eq!(reread.name, "Premium T-Shirt");
    assert_eq!(reread.status, ProductStatus::Archived);
    assert_eq!(reread.description, None);
    assert_eq!(reread.dimensions, Some(Dimensions { width_cm: 31.5, height_cm: 41.0 }));

    // Delete.
    assert_eq!(Product::delete(&db, product.id).await.unwrap(), 1);
    assert_eq!(Product::delete(&db, product.id).await.unwrap(), 0);
    assert!(Product::fetch(&db, product.id).await.unwrap().is_none());
    assert_eq!(Product::count(&db).await.unwrap(), 0);
}

#[tokio::test]
async fn foreign_key_cascade_from_generated_ddl() {
    let db = setup().await;

    let event = insert!(Event, &db, name = "Autumn Fair".to_string()).await.unwrap();
    insert!(Product, &db, slug = "mug".to_string(), name = "Mug".to_string(), event_id = event.id)
        .await
        .unwrap();
    assert_eq!(Product::count(&db).await.unwrap(), 1);

    // ON DELETE CASCADE came from #[orm(references(Event, on_delete = cascade))].
    Event::delete(&db, event.id).await.unwrap();
    assert_eq!(Product::count(&db).await.unwrap(), 0);
}

#[tokio::test]
async fn db_enum_string_contract() {
    // DB value == JSON value == form value == Display.
    assert_eq!(ProductStatus::Draft.as_str(), "draft");
    assert_eq!("published".parse::<ProductStatus>().unwrap(), ProductStatus::Published);
    assert!("bogus".parse::<ProductStatus>().is_err());
    assert_eq!(ProductStatus::Archived.to_string(), "archived");
    assert_eq!(serde_json::to_string(&ProductStatus::Draft).unwrap(), "\"draft\"");
    assert_eq!(
        serde_json::from_str::<ProductStatus>("\"archived\"").unwrap(),
        ProductStatus::Archived
    );
    assert_eq!(
        ProductStatus::VARIANTS,
        [ProductStatus::Draft, ProductStatus::Published, ProductStatus::Archived]
    );

    // The unique-slug constraint from #[orm(unique)] is enforced by the DDL.
    let db = setup().await;
    let event = insert!(Event, &db, name = "Fair".to_string()).await.unwrap();
    insert!(Product, &db, slug = "cap".to_string(), name = "Cap".to_string(), event_id = event.id)
        .await
        .unwrap();
    let duplicate = insert!(
        Product,
        &db,
        slug = "cap".to_string(),
        name = "Cap 2".to_string(),
        event_id = event.id
    )
    .await;
    assert!(duplicate.is_err());
}
