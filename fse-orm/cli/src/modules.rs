//! Module crates from the app's point of view: discovery via `cargo
//! metadata`, their shipped schema snapshots for `fse migrate`, and
//! `fse sync` — copying their `frontend/` sources into `.fse/modules/` where
//! the app's Astro build layers them in.

use std::fs;
use std::path::{Path, PathBuf};

use color_eyre::eyre::{Result, WrapErr, bail, eyre};
use fse_schema::{Schema, snapshot};

use crate::config::OrmConfig;

pub struct ModuleInfo {
    pub name: String,
    /// The crate's source directory (inside the cargo registry cache for
    /// published modules, a local path for path dependencies).
    pub dir: PathBuf,
}

/// Locates every configured module crate through `cargo metadata`. Requires
/// each to be an actual dependency of the app.
pub fn discover(root: &Path, cfg: &OrmConfig) -> Result<Vec<ModuleInfo>> {
    if cfg.modules.is_empty() {
        return Ok(Vec::new());
    }
    let output = std::process::Command::new("cargo")
        .args(["metadata", "--format-version", "1"])
        .current_dir(root)
        .output()
        .wrap_err("cannot run cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).wrap_err("cargo metadata output")?;
    let packages = meta["packages"]
        .as_array()
        .ok_or_else(|| eyre!("cargo metadata output has no packages"))?;

    let mut modules = Vec::new();
    for name in &cfg.modules {
        let package = packages
            .iter()
            .find(|p| p["name"].as_str() == Some(name))
            .ok_or_else(|| {
                eyre!("module crate `{name}` not found — is it a dependency in Cargo.toml?")
            })?;
        let manifest = package["manifest_path"]
            .as_str()
            .ok_or_else(|| eyre!("module `{name}`: no manifest_path"))?;
        let dir = PathBuf::from(manifest)
            .parent()
            .ok_or_else(|| eyre!("module `{name}`: bad manifest_path"))?
            .to_path_buf();
        modules.push(ModuleInfo {
            name: name.clone(),
            dir,
        });
    }
    Ok(modules)
}

/// A module's shipped schema snapshot — the tables it contributes.
pub fn load_schema(module: &ModuleInfo) -> Result<Schema> {
    let path = module.dir.join(".fse/schema.json");
    let raw = fs::read_to_string(&path).map_err(|_| {
        eyre!(
            "module `{}` ships no schema snapshot ({}) — the module author must run \
             `fse migrate` and include .fse/schema.json in the published crate",
            module.name,
            path.display()
        )
    })?;
    Ok(snapshot::schema_from_json(&raw)?)
}

/// `fse sync`: refreshes `.fse/modules/<name>/frontend/` from every
/// configured module's `frontend/` sources. The whole `.fse/modules/`
/// directory is regenerated (it's build output — removed modules disappear).
pub fn sync(root: &Path, cfg: &OrmConfig) -> Result<()> {
    let modules = discover(root, cfg)?;
    let base = root.join(".fse/modules");
    if base.exists() {
        fs::remove_dir_all(&base).wrap_err_with(|| format!("cannot clear {}", base.display()))?;
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

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).wrap_err_with(|| dest.display().to_string())?;
    for entry in fs::read_dir(src).wrap_err_with(|| src.display().to_string())? {
        let entry = entry.wrap_err_with(|| src.display().to_string())?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to).wrap_err_with(|| from.display().to_string())?;
        }
    }
    Ok(())
}
