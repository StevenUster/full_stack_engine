//! Integration tests for the migrate flow: real temp projects, real SQLite
//! databases, the full generate → apply → evolve cycle.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fse_cli::migrate::{MigrateOpts, run};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn project(tables: &[(&str, &str)]) -> (PathBuf, MigrateOpts) {
    let root = std::env::temp_dir().join(format!(
        "fse-cli-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    write_tables(&root, tables);
    let opts = MigrateOpts {
        assume_yes: true,
        no_prepare: true,
        database_url: Some(format!("sqlite://{}", root.join("app.db").display())),
        ..MigrateOpts::default()
    };
    (root, opts)
}

fn write_tables(root: &Path, tables: &[(&str, &str)]) {
    let dir = root.join("src/tables");
    fs::create_dir_all(&dir).unwrap();
    for (name, code) in tables {
        fs::write(dir.join(name), code).unwrap();
    }
}

fn migration_files(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root.join("migrations"))
        .map(|it| it.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

async fn table_columns(opts: &MigrateOpts, table: &str) -> Vec<String> {
    let pool = sqlx::SqlitePool::connect(opts.database_url.as_deref().unwrap())
        .await
        .unwrap();
    let rows: Vec<(String,)> = sqlx::query_as(&format!("SELECT name FROM pragma_table_info('{table}')"))
        .fetch_all(&pool)
        .await
        .unwrap();
    pool.close().await;
    rows.into_iter().map(|r| r.0).collect()
}

const EVENT: &str = "
#[derive(Table)]
pub struct Event {
    pub id: i64,
    pub name: String,
}
";

const EVENT_WITH_LOCATION: &str = "
#[derive(Table)]
pub struct Event {
    pub id: i64,
    pub name: String,
    pub location: Option<String>,
}
";

#[tokio::test]
async fn generates_applies_and_stays_idempotent() {
    let (root, opts) = project(&[("event.rs", EVENT)]);

    // First run: creates the table.
    let out = run(&root, &opts).await.unwrap();
    let generated = out.generated.expect("migration generated");
    assert!(generated.file_name().unwrap().to_string_lossy().ends_with("_create_events.sql"));
    assert!(root.join(".fse/schema.json").exists());
    assert_eq!(table_columns(&opts, "events").await, ["id", "name"]);

    // Second run with no changes: nothing generated, nothing breaks.
    let out = run(&root, &opts).await.unwrap();
    assert!(out.generated.is_none());
    assert_eq!(migration_files(&root).len(), 1);

    // Evolve the struct: plain ALTER migration, applied.
    write_tables(&root, &[("event.rs", EVENT_WITH_LOCATION)]);
    let out = run(&root, &opts).await.unwrap();
    let sql = fs::read_to_string(out.generated.unwrap()).unwrap();
    assert_eq!(sql, "ALTER TABLE events ADD COLUMN location TEXT;\n");
    assert_eq!(migration_files(&root).len(), 2);
    assert_eq!(table_columns(&opts, "events").await, ["id", "name", "location"]);
}

#[tokio::test]
async fn manual_edit_flow_applies_only_after_rerun() {
    let (root, opts) = project(&[("event.rs", EVENT)]);
    run(&root, &opts).await.unwrap();

    // A new NOT NULL column without default needs a hand-written backfill.
    write_tables(&root, &[(
        "event.rs",
        "
#[derive(Table)]
pub struct Event {
    pub id: i64,
    pub name: String,
    pub slug: String,
}
",
    )]);
    let out = run(&root, &opts).await.unwrap();
    assert!(out.needs_manual_edit);
    let file = out.generated.unwrap();
    let sql = fs::read_to_string(&file).unwrap();
    assert!(sql.contains("TODO"));
    // Not applied yet: the database still has the old shape.
    assert_eq!(table_columns(&opts, "events").await, ["id", "name"]);

    // Edit the TODO the way a user would, then rerun: applied, no new file.
    fs::write(&file, sql.replace("NULL /* TODO: backfill NOT NULL column slug */", "name")).unwrap();
    let out = run(&root, &opts).await.unwrap();
    assert!(out.generated.is_none());
    assert_eq!(table_columns(&opts, "events").await, ["id", "name", "slug"]);
}

#[tokio::test]
async fn required_columns_contract_is_enforced() {
    let (root, opts) = project(&[("event.rs", EVENT)]);
    fs::write(
        root.join("fse.toml"),
        "[orm.required_columns]\nusers = [\"id\", \"email\", \"password\"]\n",
    )
    .unwrap();

    // No users table at all.
    let err = run(&root, &opts).await.unwrap_err();
    assert!(err.message.contains("`users` table"), "got: {err}");

    // Users table present but missing a required column.
    write_tables(&root, &[(
        "user.rs",
        "
#[derive(Table)]
pub struct User {
    pub id: i64,
    pub email: String,
}
",
    )]);
    let err = run(&root, &opts).await.unwrap_err();
    assert!(err.message.contains("`password`"), "got: {err}");
}

#[tokio::test]
async fn dry_run_writes_nothing() {
    let (root, mut opts) = project(&[("event.rs", EVENT)]);
    opts.dry_run = true;
    let out = run(&root, &opts).await.unwrap();
    assert!(out.generated.is_none());
    assert!(!root.join("migrations").exists());
    assert!(!root.join(".fse").exists());
    assert!(!root.join("app.db").exists());
}
