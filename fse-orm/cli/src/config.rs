//! `fse.toml` — all optional, all defaulted to the starter layout, so a
//! fresh app needs no config file at all. Nothing app-specific lives in the
//! CLI itself: even the framework's required-columns contract arrives here.

use std::collections::BTreeMap;
use std::path::Path;

use fse_schema::Error;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct OrmConfig {
    /// Folder holding one `#[derive(Table)]` struct per file.
    pub tables_dir: String,
    /// Plain sqlx migrations folder; generated and hand-written migrations
    /// interleave by timestamp.
    pub migrations_dir: String,
    /// Committed snapshot of the schema the generated migrations produce.
    pub snapshot_path: String,
    /// Env var holding the database URL.
    pub database_url_env: String,
    /// Columns that must exist, per table — e.g. the framework's auth
    /// contract on `users`. Shipped in the starter template, not hardcoded.
    pub required_columns: BTreeMap<String, Vec<String>>,
}

impl Default for OrmConfig {
    fn default() -> Self {
        Self {
            tables_dir: "src/tables".into(),
            migrations_dir: "migrations".into(),
            snapshot_path: ".fse/schema.json".into(),
            database_url_env: "DATABASE_URL".into(),
            required_columns: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct FseToml {
    #[serde(default)]
    orm: OrmConfig,
}

pub fn load(root: &Path) -> Result<OrmConfig, Error> {
    let path = root.join("fse.toml");
    if !path.exists() {
        return Ok(OrmConfig::default());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| Error::new(format!("cannot read {}: {e}", path.display())))?;
    let parsed: FseToml =
        toml::from_str(&raw).map_err(|e| Error::new(format!("fse.toml: {e}")))?;
    Ok(parsed.orm)
}
