//! Table lookup for the query macros. `find!(Product, ...)` needs Product's
//! columns, but a function-like macro only sees tokens — so it parses the
//! tables folder itself, with the same fse-schema parser the derive and the
//! CLI use. Same-crate file reads are safe: editing a table file recompiles
//! the whole crate, so every macro re-expands.

use std::path::PathBuf;

use fse_schema::TableDef;
use proc_macro2::Span;

pub fn load_table(struct_name: &str, span: Span) -> syn::Result<TableDef> {
    let manifest: PathBuf = std::env::var("CARGO_MANIFEST_DIR")
        .map_err(|_| syn::Error::new(span, "CARGO_MANIFEST_DIR is not set"))?
        .into();
    let tables_dir = manifest.join(tables_dir_from_config(&manifest));
    if !tables_dir.exists() {
        return Err(syn::Error::new(
            span,
            format!(
                "tables folder {} does not exist (set orm.tables_dir in fse.toml)",
                tables_dir.display()
            ),
        ));
    }

    let mut sources = Vec::new();
    let entries = std::fs::read_dir(&tables_dir)
        .map_err(|e| syn::Error::new(span, format!("{}: {e}", tables_dir.display())))?;
    for entry in entries {
        let path = entry.map_err(|e| syn::Error::new(span, e.to_string()))?.path();
        if path.extension().is_some_and(|e| e == "rs") {
            let code = std::fs::read_to_string(&path)
                .map_err(|e| syn::Error::new(span, format!("{}: {e}", path.display())))?;
            sources.push((path.file_name().unwrap().to_string_lossy().into_owned(), code));
        }
    }
    sources.sort();

    let schema = fse_schema::parse::parse_sources(&sources)
        .map_err(|e| syn::Error::new(span, e.message))?;
    schema
        .tables
        .into_iter()
        .find(|t| t.struct_name == struct_name)
        .ok_or_else(|| {
            syn::Error::new(
                span,
                format!(
                    "no #[derive(Table)] struct named `{struct_name}` in {}",
                    tables_dir.display()
                ),
            )
        })
}

/// `orm.tables_dir` from fse.toml, defaulting to the starter layout.
fn tables_dir_from_config(manifest: &std::path::Path) -> String {
    let default = "src/tables".to_string();
    let Ok(raw) = std::fs::read_to_string(manifest.join("fse.toml")) else {
        return default;
    };
    let Ok(value) = raw.parse::<toml::Value>() else {
        return default;
    };
    value
        .get("orm")
        .and_then(|orm| orm.get("tables_dir"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or(default)
}
