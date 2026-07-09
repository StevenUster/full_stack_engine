//! Native replacement for `cargo sqlx prepare`, so a user of the framework
//! never needs `sqlx-cli` installed — just `fse`.
//!
//! Mechanism (see sqlx-macros-core's `query/mod.rs`): when a `query!`/
//! `query_as!`/`query_scalar!` call expands against a *live* `DATABASE_URL`
//! (i.e. not `SQLX_OFFLINE=true`), it writes its resolved metadata into
//! `SQLX_OFFLINE_DIR` as a side effect, if that env var points at an
//! existing directory. So all `cargo sqlx prepare` does — and all this
//! does — is: clear the old cache, force every query!-family call site to
//! re-expand (cargo's fingerprinting has no way to know an env var changed,
//! so source files are touched to force it), and run `cargo check` with
//! that env var set.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use fse_schema::Error;

use crate::config::{self, OrmConfig};

pub fn run(root: &Path, cfg: &OrmConfig, database_url: Option<&str>) -> Result<(), Error> {
    let url = config::resolve_database_url(root, cfg, database_url)?;

    let cache_dir = root.join(".sqlx");
    fs::create_dir_all(&cache_dir)
        .map_err(|e| Error::new(format!("cannot create {}: {e}", cache_dir.display())))?;

    // Only delete our own query-*.json files, never touch anything else a
    // user may have placed in .sqlx.
    for file in query_files(&cache_dir)? {
        fs::remove_file(&file)
            .map_err(|e| Error::new(format!("cannot remove {}: {e}", file.display())))?;
    }

    touch_rs_files(&root.join("src"))?;

    let cache_dir_abs = cache_dir
        .canonicalize()
        .map_err(|e| Error::new(format!("cannot resolve {}: {e}", cache_dir.display())))?;

    println!("refreshing query cache ...");
    let status = Command::new("cargo")
        .arg("check")
        .current_dir(root)
        .env("DATABASE_URL", &url)
        .env("SQLX_OFFLINE", "false")
        .env("SQLX_OFFLINE_DIR", &cache_dir_abs)
        .status()
        .map_err(|e| Error::new(format!("failed to run `cargo check`: {e}")))?;

    if !status.success() {
        return Err(Error::new(
            "`cargo check` failed while refreshing the query cache — fix the build error and rerun",
        ));
    }

    let count = query_files(&cache_dir)?.len();
    if count == 0 {
        println!(
            "warning: no queries found — nothing written to .sqlx (no find!/insert!/update!/query! call sites?)"
        );
    } else {
        let plural = if count == 1 { "query" } else { "queries" };
        println!(
            "wrote {count} {plural} to .sqlx — commit this directory so Docker builds work without a live database."
        );
    }
    Ok(())
}

fn query_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| Error::new(format!("{}: {e}", dir.display())))? {
        let path = entry.map_err(|e| Error::new(e.to_string()))?.path();
        let is_query_file = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("query-") && n.ends_with(".json"));
        if is_query_file {
            out.push(path);
        }
    }
    Ok(out)
}

/// Bumps the mtime of every `.rs` file under `dir` so `cargo check` treats
/// them as changed and re-expands their macros, including `query!`-family
/// calls whose SQL text hasn't changed — cargo's fingerprint has no way to
/// know `SQLX_OFFLINE_DIR` changed, and would otherwise skip them via
/// incremental compilation, silently never running the capture side effect.
fn touch_rs_files(dir: &Path) -> Result<(), Error> {
    if !dir.exists() {
        return Ok(());
    }
    let now = SystemTime::now();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in
            fs::read_dir(&current).map_err(|e| Error::new(format!("{}: {e}", current.display())))?
        {
            let path = entry.map_err(|e| Error::new(e.to_string()))?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let file = fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(|e| Error::new(format!("cannot open {}: {e}", path.display())))?;
                file.set_modified(now)
                    .map_err(|e| Error::new(format!("cannot touch {}: {e}", path.display())))?;
            }
        }
    }
    Ok(())
}
