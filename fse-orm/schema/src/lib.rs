//! fse-schema — the shared schema layer of the fse ORM.
//!
//! One schema model with three consumers:
//! - `fse-orm-macros` parses a single `#[derive(Table)]` struct into a
//!   [`TableDef`] to generate compile-time-checked sqlx queries,
//! - `fse-cli` parses the whole tables folder into a [`Schema`], diffs it
//!   against the committed snapshot and emits plain sqlx migration files,
//! - the snapshot file (`.fse/schema.json`) is just this model as JSON.
//!
//! Nothing in this crate knows about any concrete application: table names,
//! column names and constraints all come from the parsed structs.

pub mod diff;
pub mod model;
pub mod parse;
pub mod snapshot;
pub mod sql;

mod error;

pub use diff::{Migration, diff_schemas};
pub use error::Error;
pub use model::{
    ColumnDef, DefaultValue, EnumDef, ForeignKey, OnDelete, Schema, SqlType, TableDef,
};
