//! Security check: classic SQL-injection payloads fed as *values* through the
//! dynamic builder and the checked macros must be treated as data, never as
//! SQL. If any placeholder were string-interpolated, one of these would drop a
//! table, return the wrong rows, or blow up parsing.

use std::sync::atomic::{AtomicU64, Ordering};

use fse_orm::{find_one, insert, update};
use tests_app::tables::event::Event;
use tests_app::tables::product::{Product, ProductStatus};

static NEXT_DB: AtomicU64 = AtomicU64::new(0);

async fn setup() -> sqlx::SqlitePool {
    let copy = std::env::temp_dir().join(format!(
        "fse-orm-injection-{}-{}.db",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy("db/test.db", &copy).expect("template db from build.rs");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&copy)
        .foreign_keys(true);
    let db = sqlx::SqlitePool::connect_with(options).await.unwrap();

    let event = insert!(Event, &db, name = "Fair".to_string())
        .await
        .unwrap();
    for (slug, name, status, price) in [
        ("blue-shirt", "Blue Shirt", ProductStatus::Published, 20.0),
        ("red-shirt", "Red Shirt", ProductStatus::Published, 25.0),
        ("mug", "Coffee Mug", ProductStatus::Published, 8.0),
    ] {
        insert!(
            Product,
            &db,
            slug = slug.to_string(),
            name = name.to_string(),
            event_id = event.id,
            status = status,
            price = price
        )
        .await
        .unwrap();
    }
    db
}

/// A grab-bag of payloads that break a naively interpolated query.
const PAYLOADS: &[&str] = &[
    "'; DROP TABLE products; --",
    "' OR '1'='1",
    "' OR 1=1 --",
    "\"; DROP TABLE products; --",
    "mug'); DELETE FROM products WHERE ('1'='1",
    "\\'; DROP TABLE products; --",
    "1); DROP TABLE products;--",
];

async fn products_still_intact(db: &sqlx::SqlitePool) {
    let count = Product::find()
        .count(db)
        .await
        .expect("products table survives");
    assert_eq!(count, 3, "row count changed — a payload executed as SQL");
}

#[tokio::test]
async fn builder_eq_treats_payload_as_data() {
    let db = setup().await;
    for payload in PAYLOADS {
        // eq on a unique slug: a payload matches no real row, and definitely
        // must not run as SQL.
        let hits = Product::find()
            .filter(Product::SLUG.eq(payload.to_string()))
            .fetch_all(&db)
            .await
            .expect("query runs, payload bound as a value");
        assert!(
            hits.is_empty(),
            "payload {payload:?} matched a row via eq()"
        );
        products_still_intact(&db).await;
    }
}

#[tokio::test]
async fn builder_like_and_in_treat_payload_as_data() {
    let db = setup().await;
    for payload in PAYLOADS {
        let contains = Product::find()
            .filter(Product::NAME.contains(payload.to_string()))
            .fetch_all(&db)
            .await
            .expect("contains payload bound");
        assert!(
            contains.is_empty(),
            "payload {payload:?} matched via contains()"
        );

        let in_list = Product::find()
            .filter(Product::SLUG.in_(vec![payload.to_string(), "mug".into()]))
            .fetch_all(&db)
            .await
            .expect("in_ payload bound");
        // Only the legitimate "mug" should ever match.
        assert!(in_list.iter().all(|p| p.slug == "mug"));
        products_still_intact(&db).await;
    }
}

#[tokio::test]
async fn builder_update_and_delete_treat_payload_as_data() {
    let db = setup().await;
    for payload in PAYLOADS {
        // A payload as the WHERE value must update/delete nothing.
        let updated = Product::update_set()
            .set(Product::NAME, "hacked".to_string())
            .filter(Product::SLUG.eq(payload.to_string()))
            .execute(&db)
            .await
            .expect("update payload bound");
        assert_eq!(updated, 0, "payload {payload:?} updated rows");

        let deleted = Product::delete_where()
            .filter(Product::SLUG.eq(payload.to_string()))
            .execute(&db)
            .await
            .expect("delete payload bound");
        assert_eq!(deleted, 0, "payload {payload:?} deleted rows");
        products_still_intact(&db).await;
    }

    // And a payload stored as data round-trips verbatim.
    let event = find_one!(Event, &db, all).await.unwrap().unwrap();
    let stored = insert!(
        Product,
        &db,
        slug = "'; DROP TABLE products; --".to_string(),
        name = "' OR 1=1 --".to_string(),
        event_id = event.id,
        status = ProductStatus::Draft,
        price = 1.0
    )
    .await
    .expect("payload stored as a literal value");
    assert_eq!(stored.name, "' OR 1=1 --");
    let fetched = Product::fetch_by_slug(&db, "'; DROP TABLE products; --")
        .await
        .unwrap()
        .expect("row round-trips by its literal slug");
    assert_eq!(fetched.id, stored.id);
}

#[tokio::test]
async fn checked_macros_treat_payload_as_data() {
    let db = setup().await;
    for payload in PAYLOADS {
        let hit = find_one!(Product, &db, slug == *payload)
            .await
            .expect("checked query runs");
        assert!(hit.is_none(), "payload {payload:?} matched via find_one!");

        let n = update!(
            Product,
            &db,
            slug == *payload;
            name = "hacked".to_string()
        )
        .await
        .expect("checked update runs");
        assert_eq!(n, 0, "payload {payload:?} updated rows via update!");
        products_still_intact(&db).await;
    }
}
