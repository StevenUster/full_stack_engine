//! Schema diffing: old snapshot + new structs → one migration file.
//!
//! SQLite only supports ADD COLUMN, DROP COLUMN and RENAME COLUMN directly —
//! and even those with restrictions. Anything else (type change, adding
//! NOT NULL/UNIQUE, changing a default, FK or CHECK) is emitted as the
//! standard rebuild dance: create the new shape under a temporary name, copy
//! the rows across, drop the old table, rename.

use crate::error::Error;
use crate::model::{ColumnDef, DefaultValue, Schema, TableDef};
use crate::sql::{column_sql, composite_index_name, create_table_sql, index_name, quote_ident};

#[derive(Debug, Clone)]
pub struct Migration {
    /// The full migration file body (plain SQL, sqlx-compatible).
    pub sql: String,
    /// Human-readable one-liner, e.g. `products: add archived_at`.
    pub summary: String,
    /// True when data is lost (dropped tables or columns).
    pub destructive: bool,
    /// True when the generated SQL contains a TODO the user must edit before
    /// applying (a new NOT NULL column without a default needs a backfill).
    pub needs_manual_edit: bool,
}

impl Migration {
    /// `summary` reduced to a safe migration-file name fragment.
    pub fn filename_slug(&self) -> String {
        let mut slug = String::new();
        for ch in self.summary.chars() {
            if ch.is_ascii_alphanumeric() {
                slug.push(ch.to_ascii_lowercase());
            } else if !slug.ends_with('_') && !slug.is_empty() {
                slug.push('_');
            }
        }
        let slug = slug.trim_matches('_').to_string();
        slug.chars().take(48).collect()
    }
}

/// Diff two schemas. Returns `None` when they are identical.
pub fn diff_schemas(old: &Schema, new: &Schema) -> Result<Option<Migration>, Error> {
    let mut stmts: Vec<String> = Vec::new();
    let mut summary: Vec<String> = Vec::new();
    let mut destructive = false;
    let mut needs_manual_edit = false;

    for t in &new.tables {
        if old.table(&t.name).is_none() {
            stmts.push(create_table_sql(t));
            stmts.extend(crate::sql::index_sqls(t));
            stmts.extend(crate::sql::composite_index_sqls(t));
            summary.push(format!("create {}", t.name));
        }
    }
    for t in &old.tables {
        if new.table(&t.name).is_none() {
            stmts.push(format!("DROP TABLE {};", quote_ident(&t.name)));
            summary.push(format!("drop {}", t.name));
            destructive = true;
        }
    }
    for t in &new.tables {
        if let Some(old_t) = old.table(&t.name) {
            diff_table(
                old_t,
                t,
                &mut stmts,
                &mut summary,
                &mut destructive,
                &mut needs_manual_edit,
            )?;
        }
    }

    if stmts.is_empty() {
        return Ok(None);
    }
    Ok(Some(Migration {
        sql: stmts.join("\n\n") + "\n",
        summary: summary.join("; "),
        destructive,
        needs_manual_edit,
    }))
}

fn diff_table(
    old: &TableDef,
    new: &TableDef,
    stmts: &mut Vec<String>,
    summary: &mut Vec<String>,
    destructive: &mut bool,
    needs_manual_edit: &mut bool,
) -> Result<(), Error> {
    // Apply pending renames to a copy of the old table so the rest of the
    // diff compares like-for-like. A rename marker whose source column no
    // longer exists (attribute left in place after the migration ran) is
    // ignored.
    let mut eff = old.clone();
    let mut renames: Vec<(String, String)> = Vec::new();
    for c in &new.columns {
        if let Some(from) = &c.renamed_from
            && eff.column(&c.name).is_none()
            && let Some(oc) = eff.columns.iter_mut().find(|oc| oc.name == *from)
        {
            oc.name = c.name.clone();
            renames.push((from.clone(), c.name.clone()));
        }
    }

    let added: Vec<&ColumnDef> = new
        .columns
        .iter()
        .filter(|c| eff.column(&c.name).is_none())
        .collect();
    let dropped: Vec<ColumnDef> = eff
        .columns
        .iter()
        .filter(|c| new.column(&c.name).is_none())
        .cloned()
        .collect();
    let changed: Vec<String> = new
        .columns
        .iter()
        .filter(|c| {
            eff.column(&c.name)
                .is_some_and(|oc| oc.signature() != c.signature())
        })
        .map(|c| c.name.clone())
        .collect();
    // Index toggles on otherwise-unchanged columns: plain CREATE/DROP INDEX.
    let index_changes: Vec<(&ColumnDef, bool)> = new
        .columns
        .iter()
        .filter_map(|c| {
            let old_c = eff.column(&c.name)?;
            (old_c.index != c.index && old_c.signature() == c.signature()).then_some((c, c.index))
        })
        .collect();

    // Composite unique/index changes: also plain CREATE/DROP INDEX, never a
    // rebuild by themselves (same reasoning as single-column `index_changes`).
    let unique_added: Vec<&Vec<String>> = new
        .composite_uniques
        .iter()
        .filter(|c| !old.composite_uniques.contains(c))
        .collect();
    let unique_removed: Vec<&Vec<String>> = old
        .composite_uniques
        .iter()
        .filter(|c| !new.composite_uniques.contains(c))
        .collect();
    let index_added: Vec<&Vec<String>> = new
        .composite_indexes
        .iter()
        .filter(|c| !old.composite_indexes.contains(c))
        .collect();
    let index_removed: Vec<&Vec<String>> = old
        .composite_indexes
        .iter()
        .filter(|c| !new.composite_indexes.contains(c))
        .collect();

    if renames.is_empty()
        && added.is_empty()
        && dropped.is_empty()
        && changed.is_empty()
        && index_changes.is_empty()
        && unique_added.is_empty()
        && unique_removed.is_empty()
        && index_added.is_empty()
        && index_removed.is_empty()
    {
        return Ok(());
    }

    let mut bits: Vec<String> = Vec::new();
    bits.extend(renames.iter().map(|(f, t)| format!("rename {f} -> {t}")));
    bits.extend(added.iter().map(|c| format!("add {}", c.name)));
    bits.extend(dropped.iter().map(|c| format!("drop {}", c.name)));
    bits.extend(changed.iter().map(|c| format!("change {c}")));
    bits.extend(
        index_changes
            .iter()
            .map(|(c, on)| format!("{} {}", if *on { "index" } else { "unindex" }, c.name)),
    );
    bits.extend(
        unique_added
            .iter()
            .map(|c| format!("unique({})", c.join(","))),
    );
    bits.extend(
        unique_removed
            .iter()
            .map(|c| format!("drop unique({})", c.join(","))),
    );
    bits.extend(
        index_added
            .iter()
            .map(|c| format!("index({})", c.join(","))),
    );
    bits.extend(
        index_removed
            .iter()
            .map(|c| format!("drop index({})", c.join(","))),
    );

    let rebuild = !changed.is_empty()
        || added.iter().any(|c| !can_add_column(c))
        || dropped.iter().any(|c| !can_drop_column(c));

    if rebuild {
        // The rebuild drops the old table (and its indexes) and recreates
        // everything, so index changes need no separate statements.
        stmts.push(rebuild_table_sql(old, new, needs_manual_edit));
        stmts.extend(crate::sql::index_sqls(new));
        stmts.extend(crate::sql::composite_index_sqls(new));
        summary.push(format!("rebuild {} ({})", new.name, bits.join(", ")));
    } else {
        for (from, to) in &renames {
            // SQLite keeps an index working across a column rename but keeps
            // its old name; recreate it under the conventional name.
            if let Some(c) = new.column(to)
                && c.index
            {
                stmts.push(format!(
                    "DROP INDEX IF EXISTS {};",
                    quote_ident(&index_name(&new.name, from))
                ));
            }
            stmts.push(format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {};",
                quote_ident(&new.name),
                quote_ident(from),
                quote_ident(to)
            ));
            if let Some(c) = new.column(to)
                && c.index
            {
                stmts.push(create_index_sql(&new.name, &c.name));
            }
        }
        for c in &added {
            stmts.push(format!(
                "ALTER TABLE {} ADD COLUMN {};",
                quote_ident(&new.name),
                column_sql(c, false)
            ));
            if c.index {
                stmts.push(create_index_sql(&new.name, &c.name));
            }
        }
        for c in &dropped {
            stmts.push(format!(
                "ALTER TABLE {} DROP COLUMN {};",
                quote_ident(&new.name),
                quote_ident(&c.name)
            ));
        }
        for (c, on) in &index_changes {
            stmts.push(if *on {
                create_index_sql(&new.name, &c.name)
            } else {
                format!(
                    "DROP INDEX IF EXISTS {};",
                    quote_ident(&index_name(&new.name, &c.name))
                )
            });
        }
        for cols in &unique_removed {
            stmts.push(format!(
                "DROP INDEX IF EXISTS {};",
                quote_ident(&composite_index_name(&new.name, cols))
            ));
        }
        for cols in &unique_added {
            stmts.push(format!(
                "CREATE UNIQUE INDEX {} ON {} ({});",
                quote_ident(&composite_index_name(&new.name, cols)),
                quote_ident(&new.name),
                cols.iter()
                    .map(|c| quote_ident(c))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        for cols in &index_removed {
            stmts.push(format!(
                "DROP INDEX IF EXISTS {};",
                quote_ident(&composite_index_name(&new.name, cols))
            ));
        }
        for cols in &index_added {
            stmts.push(format!(
                "CREATE INDEX {} ON {} ({});",
                quote_ident(&composite_index_name(&new.name, cols)),
                quote_ident(&new.name),
                cols.iter()
                    .map(|c| quote_ident(c))
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        summary.push(format!("{}: {}", new.name, bits.join(", ")));
    }
    if !dropped.is_empty() {
        *destructive = true;
    }
    Ok(())
}

fn create_index_sql(table: &str, column: &str) -> String {
    format!(
        "CREATE INDEX {} ON {} ({});",
        quote_ident(&index_name(table, column)),
        quote_ident(table),
        quote_ident(column)
    )
}

/// SQLite `ALTER TABLE ... ADD COLUMN` restrictions: no PRIMARY KEY or
/// UNIQUE, NOT NULL needs a constant default, `CURRENT_TIMESTAMP` is not
/// allowed as the default, and a REFERENCES clause needs a NULL default.
fn can_add_column(c: &ColumnDef) -> bool {
    let constant_default = matches!(&c.default, Some(d) if !matches!(d, DefaultValue::Now));
    if c.primary_key || c.unique {
        return false;
    }
    if c.references.is_some() {
        return c.nullable && c.default.is_none();
    }
    c.nullable && !matches!(c.default, Some(DefaultValue::Now)) || constant_default
}

/// `DROP COLUMN` is refused by SQLite for pk/unique/indexed columns.
fn can_drop_column(c: &ColumnDef) -> bool {
    !c.primary_key && !c.unique && !c.index
}

fn rebuild_table_sql(old: &TableDef, new: &TableDef, needs_manual_edit: &mut bool) -> String {
    let table = &new.name;
    let tmp_name = format!("{table}_new");
    let tmp = TableDef {
        name: tmp_name.clone(),
        ..new.clone()
    };
    let create = create_table_sql(&tmp);

    let cols: Vec<String> = new.columns.iter().map(|c| quote_ident(&c.name)).collect();
    let exprs: Vec<String> = new
        .columns
        .iter()
        .map(|c| {
            if let Some(from) = &c.renamed_from
                && old.column(from).is_some()
            {
                return quote_ident(from);
            }
            if old.column(&c.name).is_some() {
                return quote_ident(&c.name);
            }
            if let Some(d) = &c.default {
                return d.sql();
            }
            if c.nullable {
                return "NULL".into();
            }
            *needs_manual_edit = true;
            format!("NULL /* TODO: backfill NOT NULL column {} */", c.name)
        })
        .collect();

    format!(
        "-- {table}: SQLite cannot express this change with ALTER TABLE, so the\n\
         -- table is rebuilt and its rows copied over.\n\
         {create}\n\n\
         INSERT INTO {qtmp} ({})\nSELECT {}\nFROM {qtable};\n\n\
         DROP TABLE {qtable};\n\n\
         ALTER TABLE {qtmp} RENAME TO {qtable};",
        cols.join(", "),
        exprs.join(", "),
        qtmp = quote_ident(&tmp_name),
        qtable = quote_ident(table),
    )
}
