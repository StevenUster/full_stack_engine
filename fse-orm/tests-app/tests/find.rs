//! Exercises the checked query macros against a real SQLite database —
//! including the paginated-search pattern they exist for.

use std::sync::atomic::{AtomicU64, Ordering};

use fse_orm::{count, delete, find, find_one, find_page, insert, update};
use tests_app::tables::event::Event;
use tests_app::tables::product::{Dimensions, Product, ProductStatus};

static NEXT_DB: AtomicU64 = AtomicU64::new(0);

async fn setup() -> (sqlx::SqlitePool, i64) {
    let copy = std::env::temp_dir().join(format!(
        "fse-orm-find-{}-{}.db",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy("db/test.db", &copy).expect("template db from build.rs");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&copy)
        .foreign_keys(true);
    let db = sqlx::SqlitePool::connect_with(options).await.unwrap();

    let event = insert!(Event, &db, name = "Fair".to_string()).await.unwrap();
    for (slug, name, status, description) in [
        ("blue-shirt", "Blue Shirt", ProductStatus::Published, Some("cotton")),
        ("red-shirt", "Red Shirt", ProductStatus::Published, None),
        ("mug", "Coffee Mug", ProductStatus::Published, Some("ceramic")),
        ("old-cap", "Old Cap", ProductStatus::Archived, None),
        ("secret", "Secret Draft", ProductStatus::Draft, None),
    ] {
        insert!(
            Product,
            &db,
            slug = slug.to_string(),
            name = name.to_string(),
            event_id = event.id,
            status = status,
            description = description.map(String::from)
        )
        .await
        .unwrap();
    }
    (db, event.id)
}

#[tokio::test]
async fn filters_ordering_and_limits() {
    let (db, event_id) = setup().await;

    // Enum comparison + LIKE + ordering.
    let shirts = find!(
        Product,
        &db,
        status == ProductStatus::Published && name.contains("Shirt"),
        order_by: name.asc()
    )
    .await
    .unwrap();
    assert_eq!(
        shirts.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        ["blue-shirt", "red-shirt"]
    );

    // The optional-search idiom: empty string filters nothing.
    let all_published = find!(
        Product,
        &db,
        status == ProductStatus::Published && name.contains_opt(""),
        order_by: id
    )
    .await
    .unwrap();
    assert_eq!(all_published.len(), 3);
    let only_mug =
        find!(Product, &db, status == ProductStatus::Published && name.contains_opt("mug"))
            .await
            .unwrap();
    assert_eq!(only_mug.len(), 1);
    assert_eq!(only_mug[0].slug, "mug");

    // eq_opt: None filters nothing, Some filters.
    let none: Option<i64> = None;
    assert_eq!(find!(Product, &db, event_id.eq_opt(none)).await.unwrap().len(), 5);
    assert_eq!(
        find!(Product, &db, event_id.eq_opt(Some(event_id + 999))).await.unwrap().len(),
        0
    );

    // is_null / limit / offset / all.
    let no_description = find!(Product, &db, description.is_null(), order_by: id).await.unwrap();
    assert_eq!(no_description.len(), 3);
    let paged = find!(Product, &db, all, order_by: id.desc(), limit: 2, offset: 1).await.unwrap();
    assert_eq!(paged.len(), 2);
    assert_eq!(paged[0].slug, "old-cap");
}

#[tokio::test]
async fn find_one_count_and_page() {
    let (db, _) = setup().await;

    let mug = find_one!(Product, &db, slug == "mug").await.unwrap().unwrap();
    assert_eq!(mug.name, "Coffee Mug");
    assert!(find_one!(Product, &db, slug == "nope").await.unwrap().is_none());

    assert_eq!(count!(Product, &db, status == ProductStatus::Published).await.unwrap(), 3);
    assert_eq!(count!(Product, &db, all).await.unwrap(), 5);

    // The killer feature: one filter, COUNT + page in one call.
    let page = find_page!(
        Product,
        &db,
        status == ProductStatus::Published,
        order_by: name.asc(),
        page: 1,
        per_page: 2
    )
    .await
    .unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(
        page.rows.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["Blue Shirt", "Coffee Mug"]
    );

    let page2 = find_page!(
        Product,
        &db,
        status == ProductStatus::Published,
        order_by: name.asc(),
        page: 2,
        per_page: 2
    )
    .await
    .unwrap();
    assert_eq!(page2.total, 3);
    assert_eq!(page2.rows.len(), 1);
    assert_eq!(page2.rows[0].name, "Red Shirt");

    // page 0 clamps to 1 instead of a negative offset.
    let clamped = find_page!(Product, &db, all, order_by: id, page: 0, per_page: 3)
        .await
        .unwrap();
    assert_eq!(clamped.rows.len(), 3);
}

#[tokio::test]
async fn update_and_delete_where() {
    let (db, _) = setup().await;

    // Partial update with enum + json conversions.
    let changed = update!(
        Product,
        &db,
        slug == "old-cap";
        status = ProductStatus::Draft,
        price = 4.5,
        dimensions = Some(Dimensions { width_cm: 20.0, height_cm: 10.0 })
    )
    .await
    .unwrap();
    assert_eq!(changed, 1);

    let cap = find_one!(Product, &db, slug == "old-cap").await.unwrap().unwrap();
    assert_eq!(cap.status, ProductStatus::Draft);
    assert_eq!(cap.price, 4.5);
    assert_eq!(cap.dimensions, Some(Dimensions { width_cm: 20.0, height_cm: 10.0 }));
    assert_eq!(cap.name, "Old Cap", "untouched columns keep their values");

    // Bulk update over a filter.
    let archived = update!(Product, &db, status == ProductStatus::Published; status = ProductStatus::Archived)
        .await
        .unwrap();
    assert_eq!(archived, 3);
    assert_eq!(count!(Product, &db, status == ProductStatus::Archived).await.unwrap(), 3);

    // delete! over a filter — the two drafts (secret + old-cap) survive.
    let deleted = delete!(Product, &db, status == ProductStatus::Archived).await.unwrap();
    assert_eq!(deleted, 3);
    assert_eq!(count!(Product, &db, all).await.unwrap(), 2);
}
