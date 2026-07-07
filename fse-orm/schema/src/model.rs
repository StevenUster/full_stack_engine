//! The schema model — the single source of truth for what a `#[derive(Table)]`
//! struct means in SQL. Everything is `serde`-serializable because the
//! snapshot file is this model as JSON.

use serde::{Deserialize, Serialize};

/// A whole application schema: every `#[derive(Table)]` struct and
/// `#[derive(DbEnum)]` enum found in the tables folder. Tables and enums are
/// kept sorted by name so snapshots are deterministic regardless of file
/// ordering.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Schema {
    pub tables: Vec<TableDef>,
    pub enums: Vec<EnumDef>,
}

impl Schema {
    pub fn table(&self, name: &str) -> Option<&TableDef> {
        self.tables.iter().find(|t| t.name == name)
    }
}

/// A `#[derive(DbEnum)]` enum: stored as TEXT, constrained with a CHECK.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnumDef {
    /// Rust enum name, e.g. `ProductStatus`.
    pub rust_name: String,
    /// Stored values: snake_case of the variant names.
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDef {
    /// SQL table name (`products`).
    pub name: String,
    /// Rust struct name (`Product`).
    pub struct_name: String,
    pub columns: Vec<ColumnDef>,
    /// Prisma-style relation fields: struct fields that are *not* database
    /// columns but hold a related row (`Option<OtherTable>`), populated by an
    /// eager join when a query asks for them via `include:`. Skipped by the
    /// migration diff entirely — they carry no DDL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<RelationDef>,
}

impl TableDef {
    pub fn column(&self, name: &str) -> Option<&ColumnDef> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn relation(&self, field: &str) -> Option<&RelationDef> {
        self.relations.iter().find(|r| r.field == field)
    }

    pub fn primary_key(&self) -> Vec<&ColumnDef> {
        self.columns.iter().filter(|c| c.primary_key).collect()
    }

    /// True when the pk is the conventional `id: i64` surrogate key, which
    /// maps to `INTEGER PRIMARY KEY AUTOINCREMENT`.
    pub fn auto_id(&self) -> bool {
        let pk = self.primary_key();
        pk.len() == 1 && pk[0].name == "id" && pk[0].ty == SqlType::Integer
    }
}

/// A belongs-to relation field: `#[orm(relation = fk_column)] field:
/// Option<Target>`. Traverses `local_column` (a foreign key on this table) to
/// its referenced row. When the FK column is nullable the join is a LEFT JOIN
/// and the field is `None` for rows with no parent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationDef {
    /// Struct field name (`run`, `donor`).
    pub field: String,
    /// Target struct name (`Run`), taken from the field's `Option<Target>` type.
    pub target_struct: String,
    /// Target SQL table (`runs`); resolved from `local_column`'s foreign key by
    /// [`crate::parse::parse_sources`]. Empty in single-struct (derive) contexts.
    pub target_table: String,
    /// The foreign-key column on this table the relation joins through.
    pub local_column: String,
    /// True when `local_column` is nullable → LEFT JOIN, `None` when absent.
    pub nullable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDef {
    pub name: String,
    /// The Rust type as written, with `Option` unwrapped (`NaiveDateTime`).
    pub rust_type: String,
    pub ty: SqlType,
    pub nullable: bool,
    pub primary_key: bool,
    pub unique: bool,
    /// Stored as TEXT through serde (`#[orm(json)]`).
    pub json: bool,
    /// Column holds a value stored as TEXT via `as_str()`/`FromStr`: a
    /// `#[derive(DbEnum)]` (with a CHECK from `check_in`) or an
    /// `#[orm(text)]` type (no CHECK).
    pub is_enum: bool,
    /// `#[orm(index)]` — a plain (non-unique) index on this column.
    #[serde(default)]
    pub index: bool,
    pub default: Option<DefaultValue>,
    pub references: Option<ForeignKey>,
    /// Allowed values (from the enum) — rendered as a CHECK constraint.
    pub check_in: Option<Vec<String>>,
    /// One-shot rename marker: `#[orm(renamed_from = "old")]`. Remove the
    /// attribute once the generated migration has been applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renamed_from: Option<String>,
}

impl ColumnDef {
    /// The column identity used for change detection — everything except the
    /// transient rename marker and the index flag (index changes are plain
    /// CREATE/DROP INDEX statements, never a rebuild by themselves).
    pub fn signature(&self) -> ColumnDef {
        ColumnDef {
            renamed_from: None,
            index: false,
            ..self.clone()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqlType {
    Integer,
    Real,
    Text,
    Blob,
    Boolean,
    Timestamp,
}

impl SqlType {
    pub fn sql(self) -> &'static str {
        match self {
            SqlType::Integer => "INTEGER",
            SqlType::Real => "REAL",
            SqlType::Text => "TEXT",
            SqlType::Blob => "BLOB",
            SqlType::Boolean => "BOOLEAN",
            SqlType::Timestamp => "TIMESTAMP",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DefaultValue {
    /// `#[orm(default = now)]` → `DEFAULT CURRENT_TIMESTAMP`.
    Now,
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
}

impl DefaultValue {
    pub fn sql(&self) -> String {
        match self {
            DefaultValue::Now => "CURRENT_TIMESTAMP".into(),
            DefaultValue::Int(i) => i.to_string(),
            DefaultValue::Float(f) => f.to_string(),
            DefaultValue::Text(s) => format!("'{}'", s.replace('\'', "''")),
            DefaultValue::Bool(b) => if *b { "1" } else { "0" }.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ForeignKey {
    /// Target table. Holds the referenced *struct* name straight after
    /// parsing a single struct; [`crate::parse::parse_sources`] resolves it
    /// to the table name.
    pub table: String,
    pub column: String,
    pub on_delete: Option<OnDelete>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnDelete {
    Cascade,
    SetNull,
    Restrict,
}

impl OnDelete {
    pub fn sql(self) -> &'static str {
        match self {
            OnDelete::Cascade => "CASCADE",
            OnDelete::SetNull => "SET NULL",
            OnDelete::Restrict => "RESTRICT",
        }
    }
}
