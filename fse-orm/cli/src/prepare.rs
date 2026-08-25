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
//! so source files are touched to force it), and run `cargo check --tests`
//! with that env var set.
//!
//! `--tests` (rather than a bare `cargo check`) matters because `cfg(test)`
//! is only enabled when checking test targets: integration tests under
//! `tests/` are separate targets that a plain `cargo check` never builds at
//! all, and `#[cfg(test)]` unit-test modules inside `src/` are compiled out
//! the same way. Either kind of query!-family call site — a fixture helper
//! in `tests/common/mod.rs`, or a `#[cfg(test)]` block in `src/` — would
//! silently never run its capture side effect without it. So both `src/`
//! and `tests/` need their `.rs` files touched, and `cargo check` needs
//! `--tests` to actually compile that code.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use color_eyre::eyre::{Result, WrapErr, bail};

use crate::config::{self, OrmConfig};

pub fn run(root: &Path, cfg: &OrmConfig, database_url: Option<&str>) -> Result<()> {
    let url = config::resolve_database_url(root, cfg, database_url)?;

    let cache_dir = root.join(".sqlx");
    fs::create_dir_all(&cache_dir)
        .wrap_err_with(|| format!("cannot create {}", cache_dir.display()))?;

    // Only delete our own query-*.json files, never touch anything else a
    // user may have placed in .sqlx.
    for file in query_files(&cache_dir)? {
        fs::remove_file(&file).wrap_err_with(|| format!("cannot remove {}", file.display()))?;
    }

    touch_rs_files(&root.join("src"))?;
    touch_rs_files(&root.join("tests"))?;

    let cache_dir_abs = cache_dir
        .canonicalize()
        .wrap_err_with(|| format!("cannot resolve {}", cache_dir.display()))?;

    println!("refreshing query cache ...");
    let status = Command::new("cargo")
        .arg("check")
        .arg("--tests")
        .current_dir(root)
        .env("DATABASE_URL", &url)
        .env("SQLX_OFFLINE", "false")
        .env("SQLX_OFFLINE_DIR", &cache_dir_abs)
        .status()
        .wrap_err("failed to run `cargo check`")?;

    if !status.success() {
        bail!(
            "`cargo check` failed while refreshing the query cache — fix the build error and rerun"
        );
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

fn query_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).wrap_err_with(|| dir.display().to_string())? {
        let path = entry.wrap_err_with(|| dir.display().to_string())?.path();
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
fn touch_rs_files(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let now = SystemTime::now();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current).wrap_err_with(|| current.display().to_string())? {
            let path = entry.wrap_err_with(|| current.display().to_string())?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let file = fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .wrap_err_with(|| format!("cannot open {}", path.display()))?;
                file.set_modified(now)
                    .wrap_err_with(|| format!("cannot touch {}", path.display()))?;
            }
        }
    }
    Ok(())
}
