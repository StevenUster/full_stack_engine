//! Creates db/test.db from the structs in src/tables before the crate
//! compiles, so the sqlx macros inside the derive-generated code have a real
//! database to verify against — DDL and queries come from the same structs.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=src/tables");

    let manifest: PathBuf = env::var("CARGO_MANIFEST_DIR").unwrap().into();
    let mut sources = Vec::new();
    for entry in fs::read_dir(manifest.join("src/tables")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "rs") {
            sources.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(&path).unwrap(),
            ));
        }
    }
    let schema = fse_schema::parse::parse_sources(&sources).expect("tables must parse");

    let db_dir = manifest.join("db");
    fs::create_dir_all(&db_dir).unwrap();
    let db_path = db_dir.join("test.db");
    let _ = fs::remove_file(&db_path);

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

    // Point the sqlx macros at the database we just built (absolute path, so
    // it resolves no matter where cargo is invoked from).
    println!(
        "cargo:rustc-env=DATABASE_URL=sqlite://{}",
        db_path.display()
    );
}
