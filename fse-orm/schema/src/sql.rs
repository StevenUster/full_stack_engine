//! SQLite DDL generation from the schema model. Kept dialect-shaped (every
//! statement goes through this module) so a second backend is a new module,
//! not a rewrite.

use crate::model::{ColumnDef, TableDef};

/// One column definition line. `auto_pk` says whether this table uses the
/// conventional `id: i64` surrogate key (rendered inline as
/// `INTEGER PRIMARY KEY AUTOINCREMENT`); composite keys are rendered as a
/// table constraint by [`create_table_sql`] instead.
pub fn column_sql(c: &ColumnDef, auto_pk: bool) -> String {
    let mut parts = vec![c.name.clone(), c.ty.sql().to_string()];
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
        parts.push(format!("CHECK ({} IN ({list}))", c.name));
    }
    parts.join(" ")
}

pub fn create_table_sql(t: &TableDef) -> String {
    let auto = t.auto_id();
    let mut lines: Vec<String> = t.columns.iter().map(|c| column_sql(c, auto)).collect();

    if !auto {
        let pk: Vec<String> = t.primary_key().iter().map(|c| c.name.clone()).collect();
        if !pk.is_empty() {
            lines.push(format!("PRIMARY KEY ({})", pk.join(", ")));
        }
    }
    for c in &t.columns {
        if let Some(fk) = &c.references {
            let mut line = format!("FOREIGN KEY ({}) REFERENCES {}({})", c.name, fk.table, fk.column);
            if let Some(od) = fk.on_delete {
                line.push_str(&format!(" ON DELETE {}", od.sql()));
            }
            lines.push(line);
        }
    }

    format!("CREATE TABLE {} (\n    {}\n);", t.name, lines.join(",\n    "))
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
                index_name(&t.name, &c.name),
                t.name,
                c.name
            )
        })
        .collect()
}
