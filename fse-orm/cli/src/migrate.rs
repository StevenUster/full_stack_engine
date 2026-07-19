//! The `fse migrate` flow: parse the tables folder, diff against the
//! snapshot, write a plain sqlx migration, then apply everything pending.
//!
//! The snapshot is updated at *generation* time: it records the schema the
//! generated migrations produce, while the database's `_sqlx_migrations`
//! table tracks what has been applied. So an aborted apply, or the
//! edit-the-TODO-then-rerun flow, never generates the same migration twice —
//! rerunning just applies what is pending.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use fse_schema::{Error, Schema, diff_schemas, parse, snapshot};

use crate::config::{self, OrmConfig};

#[derive(Debug, Default)]
pub struct MigrateOpts {
    /// Print the pending diff without writing or applying anything.
    pub dry_run: bool,
    /// Skip the confirmation prompts (also required for non-interactive use).
    pub assume_yes: bool,
    /// Skip `cargo sqlx prepare` after applying.
    pub no_prepare: bool,
    /// Overrides the env var from fse.toml (used by tests).
    pub database_url: Option<String>,
}

#[derive(Debug)]
pub struct MigrateOutcome {
    /// The migration file written this run, if the schema changed.
    pub generated: Option<PathBuf>,
    /// The generated SQL contains a TODO and was not applied.
    pub needs_manual_edit: bool,
}

pub async fn run(root: &Path, opts: &MigrateOpts) -> Result<MigrateOutcome, Error> {
    let cfg = config::load(root)?;
    let new_schema = parse_tables(root, &cfg)?;
    validate_required_columns(&new_schema, &cfg)?;

    let snapshot_path = root.join(&cfg.snapshot_path);
    let old_schema = if snapshot_path.exists() {
        let raw = fs::read_to_string(&snapshot_path)
            .map_err(|e| Error::new(format!("cannot read {}: {e}", snapshot_path.display())))?;
        snapshot::schema_from_json(&raw)?
    } else {
        Schema::default()
    };

    let migrations_dir = root.join(&cfg.migrations_dir);
    let mut outcome = MigrateOutcome {
        generated: None,
        needs_manual_edit: false,
    };

    if let Some(migration) = diff_schemas(&old_schema, &new_schema)? {
        println!("Schema change: {}\n", migration.summary);
        println!("{}", migration.sql);
        if migration.destructive {
            println!("!! this migration is destructive (data is dropped)\n");
        }

        if opts.dry_run {
            println!("dry run: nothing written.");
            return Ok(outcome);
        }
        if !opts.assume_yes && !confirm(migration.destructive)? {
            println!("aborted: nothing written.");
            return Ok(outcome);
        }

        fs::create_dir_all(&migrations_dir)
            .map_err(|e| Error::new(format!("cannot create {}: {e}", migrations_dir.display())))?;
        let file = migrations_dir.join(format!(
            "{}_{}.sql",
            next_version(&migrations_dir)?,
            slug_or_default(&migration.filename_slug()),
        ));
        fs::write(&file, &migration.sql)
            .map_err(|e| Error::new(format!("cannot write {}: {e}", file.display())))?;
        write_snapshot(&snapshot_path, &new_schema)?;
        println!("wrote {}", file.display());

        outcome.needs_manual_edit = migration.needs_manual_edit;
        outcome.generated = Some(file);
        if migration.needs_manual_edit {
            println!(
                "\nThe migration contains a TODO (a new NOT NULL column needs a backfill).\n\
                 Edit the file, then run `fse migrate` again to apply it."
            );
            return Ok(outcome);
        }
    } else {
        // Keep the snapshot in existence even when nothing changed (first
        // run of an app whose migrations already match its structs).
        if !snapshot_path.exists() {
            write_snapshot(&snapshot_path, &new_schema)?;
        }
        println!("schema up to date.");
    }

    if opts.dry_run {
        return Ok(outcome);
    }

    apply_pending(root, &cfg, opts, &migrations_dir).await?;

    if !opts.no_prepare {
        crate::prepare::run(root, &cfg, opts.database_url.as_deref())?;
    }
    Ok(outcome)
}

fn parse_tables(root: &Path, cfg: &OrmConfig) -> Result<Schema, Error> {
    let dir = root.join(&cfg.tables_dir);
    if !dir.exists() {
        return Err(Error::new(format!(
            "tables folder {} does not exist (set orm.tables_dir in fse.toml)",
            dir.display()
        )));
    }
    let mut sources = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))? {
        let path = entry.map_err(|e| Error::new(e.to_string()))?.path();
        if path.extension().is_some_and(|e| e == "rs") {
            sources.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                fs::read_to_string(&path)
                    .map_err(|e| Error::new(format!("{}: {e}", path.display())))?,
            ));
        }
    }
    sources.sort();
    if sources.is_empty() {
        return Err(Error::new(format!("no .rs files in {}", dir.display())));
    }
    parse::parse_sources(&sources)
}

/// The framework contract from fse.toml: every listed table must exist and
/// carry the listed columns (e.g. what auth needs on `users`).
fn validate_required_columns(schema: &Schema, cfg: &OrmConfig) -> Result<(), Error> {
    for (table_name, columns) in &cfg.required_columns {
        let Some(table) = schema.table(table_name) else {
            return Err(Error::new(format!(
                "fse.toml requires a `{table_name}` table, but no #[derive(Table)] struct defines it"
            )));
        };
        for column in columns {
            if table.column(column).is_none() {
                return Err(Error::new(format!(
                    "fse.toml requires column `{column}` on `{table_name}` — the framework depends on it; add it back to the struct"
                )));
            }
        }
    }
    Ok(())
}

fn write_snapshot(path: &Path, schema: &Schema) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::new(format!("cannot create {}: {e}", parent.display())))?;
    }
    fs::write(path, snapshot::schema_to_json(schema))
        .map_err(|e| Error::new(format!("cannot write {}: {e}", path.display())))
}

/// sqlx migration version: current UTC timestamp, bumped past any version
/// already in the folder (hand-written or generated seconds apart).
fn next_version(migrations_dir: &Path) -> Result<u64, Error> {
    let mut version: u64 = chrono::Utc::now()
        .format("%Y%m%d%H%M%S")
        .to_string()
        .parse()
        .expect("timestamp is numeric");
    let mut existing = Vec::new();
    if migrations_dir.exists() {
        for entry in fs::read_dir(migrations_dir).map_err(|e| Error::new(e.to_string()))? {
            let name = entry.map_err(|e| Error::new(e.to_string()))?.file_name();
            let name = name.to_string_lossy();
            let digits: String = name.chars().take_while(char::is_ascii_digit).collect();
            if let Ok(v) = digits.parse::<u64>() {
                existing.push(v);
            }
        }
    }
    while existing.contains(&version) {
        version += 1;
    }
    Ok(version)
}

fn slug_or_default(slug: &str) -> &str {
    if slug.is_empty() { "schema" } else { slug }
}

fn confirm(destructive: bool) -> Result<bool, Error> {
    if destructive {
        print!("apply this DESTRUCTIVE migration? type `yes` to continue: ");
    } else {
        print!("write and apply? [Y/n] ");
    }
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| Error::new(e.to_string()))?;
    let answer = answer.trim().to_lowercase();
    Ok(if destructive {
        answer == "yes"
    } else {
        answer.is_empty() || answer == "y" || answer == "yes"
    })
}

async fn apply_pending(
    root: &Path,
    cfg: &OrmConfig,
    opts: &MigrateOpts,
    migrations_dir: &Path,
) -> Result<(), Error> {
    if !migrations_dir.exists() {
        return Ok(());
    }
    let url = config::resolve_database_url(root, cfg, opts.database_url.as_deref())?;

    // Migrations must run with foreign-key enforcement OFF: a table rebuild
    // DROPs the old table, and with enforcement on that DROP fires child
    // tables' ON DELETE actions — CASCADE silently wipes their rows, RESTRICT
    // fails the migration. sqlx wraps every migration in a transaction, where
    // `PRAGMA foreign_keys` is a silent no-op, so it has to be set on the
    // connection itself. `pragma_foreign_key_check` below restores the safety
    // net once everything has been applied.
    let options = url
        .parse::<sqlx::sqlite::SqliteConnectOptions>()
        .map_err(|e| Error::new(format!("invalid database url: {e}")))?
        .create_if_missing(true)
        .foreign_keys(false);
    let pool = sqlx::SqlitePool::connect_with(options)
        .await
        .map_err(|e| Error::new(format!("cannot open database: {e}")))?;

    let migrator = sqlx::migrate::Migrator::new(migrations_dir.to_path_buf())
        .await
        .map_err(|e| Error::new(format!("invalid migrations folder: {e}")))?;
    migrator
        .run(&pool)
        .await
        .map_err(|e| Error::new(format!("migration failed: {e}")))?;

    let violations: Vec<(String, Option<i64>, String)> =
        sqlx::query_as("SELECT \"table\", rowid, parent FROM pragma_foreign_key_check")
            .fetch_all(&pool)
            .await
            .map_err(|e| Error::new(format!("foreign_key_check failed: {e}")))?;
    pool.close().await;
    if !violations.is_empty() {
        let examples: Vec<String> = violations
            .iter()
            .take(5)
            .map(|(table, rowid, parent)| match rowid {
                Some(rowid) => format!("{table} rowid {rowid} -> missing {parent} row"),
                None => format!("{table} -> missing {parent} row"),
            })
            .collect();
        return Err(Error::new(format!(
            "migrations applied, but the database now has {} foreign key violation(s):\n  {}\n\
             fix the offending rows (or the migration that orphaned them) and rerun.",
            violations.len(),
            examples.join("\n  "),
        )));
    }
    println!("database is up to date.");
    Ok(())
}
