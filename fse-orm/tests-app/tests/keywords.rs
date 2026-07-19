//! Columns named after SQL keywords (`order`, `group`) must work through
//! every code path — DDL (build.rs already created the table), the checked
//! macros and the dynamic builder — because all generated SQL quotes its
//! identifiers.

use std::sync::atomic::{AtomicU64, Ordering};

use fse_orm::{count, delete, find, find_one, find_page, insert, update};
use tests_app::tables::sort_item::SortItem;

static NEXT_DB: AtomicU64 = AtomicU64::new(0);

async fn setup() -> sqlx::SqlitePool {
    let copy = std::env::temp_dir().join(format!(
        "fse-orm-keywords-{}-{}.db",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy("db/test.db", &copy).expect("template db from build.rs");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&copy)
        .foreign_keys(true);
    let db = sqlx::SqlitePool::connect_with(options).await.unwrap();

    for (order, group) in [(3, "b"), (1, "a"), (2, "a")] {
        insert!(SortItem, &db, order = order, group = group.to_string())
            .await
            .unwrap();
    }
    db
}

#[tokio::test]
async fn checked_macros_handle_keyword_columns() {
    let db = setup().await;

    let a_items = find!(SortItem, &db, group == "a", order_by: order.asc())
        .await
        .unwrap();
    assert_eq!(a_items.iter().map(|i| i.order).collect::<Vec<_>>(), [1, 2]);

    let first = find_one!(SortItem, &db, order == 3).await.unwrap().unwrap();
    assert_eq!(first.group, "b");

    let page = find_page!(SortItem, &db, all, order_by: order.desc(), page: 1, per_page: 2)
        .await
        .unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(
        page.rows.iter().map(|i| i.order).collect::<Vec<_>>(),
        [3, 2]
    );

    assert_eq!(count!(SortItem, &db, group == "a").await.unwrap(), 2);

    let updated = update!(SortItem, &db, order == 1; group = "c".to_string())
        .await
        .unwrap();
    assert_eq!(updated, 1);

    let deleted = delete!(SortItem, &db, group == "b").await.unwrap();
    assert_eq!(deleted, 1);
    assert_eq!(SortItem::count(&db).await.unwrap(), 2);
}

#[tokio::test]
async fn derive_crud_and_builder_handle_keyword_columns() {
    let db = setup().await;

    // Derive-generated CRUD.
    let all = SortItem::fetch_all(&db).await.unwrap();
    assert_eq!(all.len(), 3);
    let mut one = SortItem::fetch(&db, all[0].id).await.unwrap().unwrap();
    one.order = 42;
    one.update(&db).await.unwrap();
    assert_eq!(
        SortItem::fetch(&db, one.id).await.unwrap().unwrap().order,
        42
    );

    // Dynamic builder: filter, order, update_set, delete_where.
    let sorted = SortItem::find()
        .filter(SortItem::GROUP.eq("a".to_string()))
        .order_by(SortItem::ORDER.desc())
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(sorted.len(), 2);

    let updated = SortItem::update_set()
        .set(SortItem::ORDER, 7i64)
        .filter(SortItem::GROUP.eq("a".to_string()))
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(updated, 2);

    let deleted = SortItem::delete_where()
        .filter(SortItem::ORDER.eq(7i64))
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(deleted, 2);
}
