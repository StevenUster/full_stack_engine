//! Creates a scratch test database from the `#[model]` structs in `tests/`
//! before the crate compiles, so the sqlx macros inside the `Table` codegen
//! those models expand to have a real database to verify against — the same
//! pattern as fse-orm's tests-app. The framework library itself contains no
//! checked queries; this exists purely for the integration tests.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=tests");

    let manifest: PathBuf = env::var("CARGO_MANIFEST_DIR").unwrap().into();
    let out_dir: PathBuf = env::var("OUT_DIR").unwrap().into();
    let db_path = out_dir.join("test.db");
    let _ = fs::remove_file(&db_path);

    // Point the sqlx macros at the database (absolute path, so it resolves no
    // matter where cargo is invoked from). Set unconditionally so the crate
    // also builds from a published package without the tests directory.
    println!(
        "cargo:rustc-env=DATABASE_URL=sqlite://{}",
        db_path.display()
    );

    let tests_dir = manifest.join("tests");
    let mut sources = Vec::new();
    if let Ok(entries) = fs::read_dir(&tests_dir) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rs") {
                sources.push((
                    path.file_name().unwrap().to_string_lossy().into_owned(),
                    fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    let schema = fse_schema::parse::parse_sources(&sources).expect("test models must parse");
    if schema.tables.is_empty() {
        return;
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);
        let pool = sqlx::SqlitePool::connect_with(options).await.unwrap();
        for table in &schema.tables {
            let ddl = fse_schema::sql::create_table_sql(table);
            sqlx::query(&ddl)
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("generated DDL failed for {}: {e}\n{ddl}", table.name));
        }
        pool.close().await;
    });
}
