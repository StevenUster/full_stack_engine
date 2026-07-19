//! Module crates from the app's point of view: discovery via `cargo
//! metadata`, their shipped schema snapshots for `fse migrate`, and
//! `fse sync` — copying their `frontend/` sources into `.fse/modules/` where
//! the app's Astro build layers them in.

use std::fs;
use std::path::{Path, PathBuf};

use fse_schema::{Error, Schema, snapshot};

use crate::config::OrmConfig;

pub struct ModuleInfo {
    pub name: String,
    /// The crate's source directory (inside the cargo registry cache for
    /// published modules, a local path for path dependencies).
    pub dir: PathBuf,
}

/// Locates every configured module crate through `cargo metadata`. Requires
/// each to be an actual dependency of the app.
pub fn discover(root: &Path, cfg: &OrmConfig) -> Result<Vec<ModuleInfo>, Error> {
    if cfg.modules.is_empty() {
        return Ok(Vec::new());
    }
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .current_dir(root)
        .output()
        .map_err(|e| Error::new(format!("cannot run cargo metadata: {e}")))?;
    if !output.status.success() {
        return Err(Error::new(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let meta: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::new(format!("cargo metadata output: {e}")))?;
    let packages = meta["packages"]
        .as_array()
        .ok_or_else(|| Error::new("cargo metadata output has no packages"))?;

    let mut modules = Vec::new();
    for name in &cfg.modules {
        let package = packages
            .iter()
            .find(|p| p["name"].as_str() == Some(name))
            .ok_or_else(|| {
                Error::new(format!(
                    "module crate `{name}` not found — is it a dependency in Cargo.toml?"
                ))
            })?;
        let manifest = package["manifest_path"]
            .as_str()
            .ok_or_else(|| Error::new(format!("module `{name}`: no manifest_path")))?;
        let dir = PathBuf::from(manifest)
            .parent()
            .ok_or_else(|| Error::new(format!("module `{name}`: bad manifest_path")))?
            .to_path_buf();
        modules.push(ModuleInfo {
            name: name.clone(),
            dir,
        });
    }
    Ok(modules)
}

/// A module's shipped schema snapshot — the tables it contributes.
pub fn load_schema(module: &ModuleInfo) -> Result<Schema, Error> {
    let path = module.dir.join(".fse/schema.json");
    let raw = fs::read_to_string(&path).map_err(|_| {
        Error::new(format!(
            "module `{}` ships no schema snapshot ({}) — the module author must run \
             `fse migrate` and include .fse/schema.json in the published crate",
            module.name,
            path.display()
        ))
    })?;
    snapshot::schema_from_json(&raw)
}

/// `fse sync`: refreshes `.fse/modules/<name>/frontend/` from every
/// configured module's `frontend/` sources. The whole `.fse/modules/`
/// directory is regenerated (it's build output — removed modules disappear).
pub fn sync(root: &Path, cfg: &OrmConfig) -> Result<(), Error> {
    let modules = discover(root, cfg)?;
    let base = root.join(".fse/modules");
    if base.exists() {
        fs::remove_dir_all(&base)
            .map_err(|e| Error::new(format!("cannot clear {}: {e}", base.display())))?;
    }
    if modules.is_empty() {
        println!("no modules configured (fse.toml [orm] modules).");
        return Ok(());
    }
    for module in &modules {
        let src = module.dir.join("frontend");
        if !src.exists() {
            println!("{}: no frontend/ sources.", module.name);
            continue;
        }
        let dest = base.join(&module.name).join("frontend");
        copy_dir(&src, &dest)?;
        println!("{}: frontend synced to {}", module.name, dest.display());
    }
    Ok(())
}

fn copy_dir(src: &Path, dest: &Path) -> Result<(), Error> {
    fs::create_dir_all(dest).map_err(|e| Error::new(format!("{}: {e}", dest.display())))?;
    for entry in fs::read_dir(src).map_err(|e| Error::new(format!("{}: {e}", src.display())))? {
        let entry = entry.map_err(|e| Error::new(e.to_string()))?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| Error::new(format!("{}: {e}", from.display())))?;
        }
    }
    Ok(())
}
