//! Untrusted input must never change a query's *shape* — not just its SQL
//! (see injection.rs) but also its semantics:
//!
//! - `%`/`_` inside a `contains`/`starts_with`/`contains_opt` value must be
//!   matched literally, not act as LIKE wildcards (a search box fed `%` must
//!   not turn a scoped filter into "match everything").
//! - A negative `per_page` must not become `LIMIT -n`, which SQLite treats as
//!   "no limit" (a full-table dump if an app wires per_page to a query param).

use std::sync::atomic::{AtomicU64, Ordering};

use fse_orm::{find, find_one, find_page, insert};
use tests_app::tables::event::Event;
use tests_app::tables::product::{Product, ProductStatus};

static NEXT_DB: AtomicU64 = AtomicU64::new(0);

async fn setup() -> sqlx::SqlitePool {
    let copy = std::env::temp_dir().join(format!(
        "fse-orm-untrusted-{}-{}.db",
        std::process::id(),
        NEXT_DB.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::copy("db/test.db", &copy).expect("template db from build.rs");
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(&copy)
        .foreign_keys(true);
    let db = sqlx::SqlitePool::connect_with(options).await.unwrap();

    let event = insert!(Event, &db, name = "Fair".to_string()).await.unwrap();
    // "100% Cotton Tee" contains a literal `%`, "a_c" a literal `_`.
    for (slug, name) in [
        ("blue-shirt", "Blue Shirt"),
        ("red-shirt", "Red Shirt"),
        ("cotton-tee", "100% Cotton Tee"),
        ("a-c", "a_c"),
    ] {
        insert!(
            Product,
            &db,
            slug = slug.to_string(),
            name = name.to_string(),
            event_id = event.id,
            status = ProductStatus::Published,
            price = 1.0
        )
        .await
        .unwrap();
    }
    db
}

// ─── LIKE wildcards in user-supplied search values ──────────────────────────

#[tokio::test]
async fn builder_contains_matches_wildcards_literally() {
    let db = setup().await;

    // `%` is not "match everything", it is the character `%`.
    let hits = Product::find()
        .filter(Product::NAME.contains("%".to_string()))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(
        hits.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        vec!["cotton-tee"],
        "`%` must only match names containing a literal percent sign"
    );

    // `_` is not "any one character", it is the character `_`.
    let hits = Product::find()
        .filter(Product::NAME.contains("a_c".to_string()))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(
        hits.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        vec!["a-c"],
        "`_` must not act as a single-character wildcard"
    );

    // A literal substring containing `%` round-trips.
    let hits = Product::find()
        .filter(Product::NAME.contains("100% Cotton".to_string()))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].slug, "cotton-tee");

    // Escape character itself stays literal too.
    let hits = Product::find()
        .filter(Product::NAME.contains("\\".to_string()))
        .fetch_all(&db)
        .await
        .unwrap();
    assert!(hits.is_empty(), "no name contains a backslash");
}

#[tokio::test]
async fn builder_starts_with_matches_wildcards_literally() {
    let db = setup().await;

    let hits = Product::find()
        .filter(Product::NAME.starts_with("%".to_string()))
        .fetch_all(&db)
        .await
        .unwrap();
    assert!(hits.is_empty(), "no name starts with a literal `%`");

    let hits = Product::find()
        .filter(Product::NAME.starts_with("100%".to_string()))
        .fetch_all(&db)
        .await
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].slug, "cotton-tee");
}

#[tokio::test]
async fn checked_contains_matches_wildcards_literally() {
    let db = setup().await;

    let payload = "%";
    let hits = find!(Product, &db, name.contains(payload)).await.unwrap();
    assert_eq!(
        hits.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        vec!["cotton-tee"],
        "checked contains: `%` must only match a literal percent sign"
    );

    let payload = "a_c";
    let hit = find_one!(Product, &db, name.contains(payload)).await.unwrap();
    assert_eq!(hit.expect("literal a_c row").slug, "a-c");

    let payload = "B_ue";
    let hit = find_one!(Product, &db, name.contains(payload)).await.unwrap();
    assert!(hit.is_none(), "`_` must not match 'Blue' one-char-wildcard style");

    let payload = "100%";
    let hit = find_one!(Product, &db, name.starts_with(payload)).await.unwrap();
    assert_eq!(hit.expect("literal 100% prefix").slug, "cotton-tee");
}

#[tokio::test]
async fn checked_contains_opt_matches_wildcards_literally_and_keeps_empty_semantics() {
    let db = setup().await;

    // Empty string still means "no filter".
    let all = find!(Product, &db, name.contains_opt("")).await.unwrap();
    assert_eq!(all.len(), 4);

    // The search-box payload `%` must not degrade into "no filter" holds too.
    let search = "%";
    let hits = find!(Product, &db, name.contains_opt(search)).await.unwrap();
    assert_eq!(
        hits.iter().map(|p| p.slug.as_str()).collect::<Vec<_>>(),
        vec!["cotton-tee"],
        "contains_opt: `%` must only match a literal percent sign"
    );
}

// ─── Negative/zero per_page must not dump the table ─────────────────────────

#[tokio::test]
async fn find_page_clamps_hostile_per_page() {
    let db = setup().await;

    // SQLite treats a negative LIMIT as "unlimited" — a per_page of -1 from an
    // unvalidated query param must not return every row.
    let page = find_page!(Product, &db, all, page: 1, per_page: -1).await.unwrap();
    assert!(
        page.rows.len() <= 1,
        "negative per_page must be clamped, got {} rows",
        page.rows.len()
    );

    let page = find_page!(Product, &db, all, page: 1, per_page: 0).await.unwrap();
    assert!(page.rows.len() <= 1, "zero per_page must be clamped");
}

#[tokio::test]
async fn builder_fetch_page_clamps_hostile_per_page() {
    let db = setup().await;

    let page = Product::find().fetch_page(&db, 1, -1).await.unwrap();
    assert!(
        page.rows.len() <= 1,
        "negative per_page must be clamped, got {} rows",
        page.rows.len()
    );

    let page = Product::find().fetch_page(&db, 1, 0).await.unwrap();
    assert!(page.rows.len() <= 1, "zero per_page must be clamped");
}
