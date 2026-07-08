//! Exercises the dynamic (unchecked) query builder — query shapes composed
//! at runtime, the way an admin list page with user-selected sort and
//! optional filters would.

use std::sync::atomic::{AtomicU64, Ordering};

use fse_orm::insert;
use tests_app::tables::event::Event;
use tests_app::tables::product::{Product, ProductStatus};

static NEXT_DB: AtomicU64 = AtomicU64::new(0);

async fn setup() -> sqlx::SqlitePool {
    let copy = std::env::temp_dir().join(format!(
        "fse-orm-dynamic-{}-{}.db",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy("db/test.db", &copy).expect("template db from build.rs");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&copy)
        .foreign_keys(true);
    let db = sqlx::SqlitePool::connect_with(options).await.unwrap();

    let event = insert!(Event, &db, name = "Fair".to_string()).await.unwrap();
    for (slug, name, status, price) in [
        ("blue-shirt", "Blue Shirt", ProductStatus::Published, 20.0),
        ("red-shirt", "Red Shirt", ProductStatus::Published, 25.0),
        ("mug", "Coffee Mug", ProductStatus::Published, 8.0),
        ("old-cap", "Old Cap", ProductStatus::Archived, 5.0),
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

#[tokio::test]
async fn runtime_composed_select() {
    let db = setup().await;

    // The shape is decided at runtime: optional search, user-picked sort.
    let search: Option<&str> = Some("Shirt");
    let sort_param = "price";

    let mut query = Product::find().filter(Product::STATUS.eq(ProductStatus::Published));
    if let Some(s) = search {
        query = query.filter(Product::NAME.contains(s));
    }
    let order = match sort_param {
        "price" => Product::PRICE.desc(),
        _ => Product::CREATED_AT.desc(),
    };
    let shirts = query.order_by(order).fetch_all(&db).await.unwrap();
    assert_eq!(
        shirts.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        ["red-shirt", "blue-shirt"]
    );

    // FromRow decodes enum and json columns like the checked queries do.
    assert_eq!(shirts[0].status, ProductStatus::Published);
    assert_eq!(shirts[0].dimensions, None);

    // or / in_ / is_not_null / range — the long tail of admin filters.
    let either = Product::find()
        .filter(Product::SLUG.eq("mug").or(Product::SLUG.eq("old-cap")))
        .order_by(Product::ID.asc())
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(either.len(), 2);

    let in_list = Product::find()
        .filter(Product::SLUG.in_(vec!["mug".to_string(), "red-shirt".to_string()]))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(in_list.len(), 2);
    assert!(Product::find()
        .filter(Product::SLUG.in_(Vec::<String>::new()))
        .fetch_all(&db)
        .await
        .unwrap()
        .is_empty());

    let cheap = Product::find()
        .filter(Product::PRICE.lt(10.0))
        .count(&db)
        .await
        .unwrap();
    assert_eq!(cheap, 2);

    let one = Product::find()
        .filter(Product::SLUG.eq("mug"))
        .fetch_optional(&db)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(one.name, "Coffee Mug");
}

#[tokio::test]
async fn pagination_and_writes() {
    let db = setup().await;

    let page = Product::find()
        .filter(Product::STATUS.eq(ProductStatus::Published))
        .order_by(Product::NAME.asc())
        .fetch_page(&db, 2, 2)
        .await
        .unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].name, "Red Shirt");

    // Dynamic partial update.
    let updated = Product::update_set()
        .set(Product::PRICE, 9.5)
        .set(Product::STATUS, ProductStatus::Draft)
        .filter(Product::SLUG.eq("mug"))
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(updated, 1);
    let mug = Product::find().filter(Product::SLUG.eq("mug")).fetch_one(&db).await.unwrap();
    assert_eq!(mug.price, 9.5);
    assert_eq!(mug.status, ProductStatus::Draft);

    // set() without filter is an error only when there is nothing to set.
    let err = Product::update_set().filter(Product::ID.eq(1i64)).execute(&db).await;
    assert!(err.is_err());

    // Dynamic delete.
    let deleted = Product::delete_where()
        .filter(Product::STATUS.eq(ProductStatus::Archived))
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(Product::find().count(&db).await.unwrap(), 3);
}
