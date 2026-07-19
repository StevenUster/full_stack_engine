//! SQLite DDL generation from the schema model. Kept dialect-shaped (every
//! statement goes through this module) so a second backend is a new module,
//! not a rewrite.

use crate::model::{ColumnDef, TableDef};

/// A table/column/index name, double-quoted for SQL. Every identifier the
/// ORM emits goes through this, so names that collide with SQL keywords
/// (`order`, `group`, `index`, ...) just work.
pub fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// One column definition line. `auto_pk` says whether this table uses the
/// conventional `id: i64` surrogate key (rendered inline as
/// `INTEGER PRIMARY KEY AUTOINCREMENT`); composite keys are rendered as a
/// table constraint by [`create_table_sql`] instead.
pub fn column_sql(c: &ColumnDef, auto_pk: bool) -> String {
    let mut parts = vec![quote_ident(&c.name), c.ty.sql().to_string()];
    if c.primary_key && auto_pk {
        parts.push("PRIMARY KEY AUTOINCREMENT".into());
    }
    if !c.nullable {
        parts.push("NOT NULL".into());
    }
    if c.unique {
        parts.push("UNIQUE".into());
    }
    if let Some(d) = &c.default {
        parts.push(format!("DEFAULT {}", d.sql()));
    }
    if let Some(values) = &c.check_in
        && !values.is_empty()
    {
        let list = values
            .iter()
            .map(|v| format!("'{}'", v.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!("CHECK ({} IN ({list}))", quote_ident(&c.name)));
    }
    parts.join(" ")
}

pub fn create_table_sql(t: &TableDef) -> String {
    let auto = t.auto_id();
    let mut lines: Vec<String> = t.columns.iter().map(|c| column_sql(c, auto)).collect();

    if !auto {
        let pk: Vec<String> = t
            .primary_key()
            .iter()
            .map(|c| quote_ident(&c.name))
            .collect();
        if !pk.is_empty() {
            lines.push(format!("PRIMARY KEY ({})", pk.join(", ")));
        }
    }
    for c in &t.columns {
        if let Some(fk) = &c.references {
            let mut line = format!(
                "FOREIGN KEY ({}) REFERENCES {}({})",
                quote_ident(&c.name),
                quote_ident(&fk.table),
                quote_ident(&fk.column)
            );
            if let Some(od) = fk.on_delete {
                line.push_str(&format!(" ON DELETE {}", od.sql()));
            }
            lines.push(line);
        }
    }

    format!(
        "CREATE TABLE {} (\n    {}\n);",
        quote_ident(&t.name),
        lines.join(",\n    ")
    )
}

pub fn index_name(table: &str, column: &str) -> String {
    format!("idx_{table}_{column}")
}

/// `CREATE INDEX` statements for every `#[orm(index)]` column.
pub fn index_sqls(t: &TableDef) -> Vec<String> {
    t.columns
        .iter()
        .filter(|c| c.index)
        .map(|c| {
            format!(
                "CREATE INDEX {} ON {} ({});",
                quote_ident(&index_name(&t.name, &c.name)),
                quote_ident(&t.name),
                quote_ident(&c.name)
            )
        })
        .collect()
}

pub fn composite_index_name(table: &str, columns: &[String]) -> String {
    format!("idx_{table}_{}", columns.join("_"))
}

/// `CREATE [UNIQUE] INDEX` statements for every struct-level `#[orm(unique(...))]`
/// / `#[orm(index(...))]` — composite constraints, enforced as indexes (not
/// inline table constraints) so they can be added/dropped without a rebuild.
pub fn composite_index_sqls(t: &TableDef) -> Vec<String> {
    let mut out: Vec<String> = t
        .composite_uniques
        .iter()
        .map(|cols| composite_index_sql(&t.name, cols, true))
        .collect();
    out.extend(
        t.composite_indexes
            .iter()
            .map(|cols| composite_index_sql(&t.name, cols, false)),
    );
    out
}

fn composite_index_sql(table: &str, columns: &[String], unique: bool) -> String {
    format!(
        "CREATE {}INDEX {} ON {} ({});",
        if unique { "UNIQUE " } else { "" },
        quote_ident(&composite_index_name(table, columns)),
        quote_ident(table),
        columns
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", "),
    )
}
