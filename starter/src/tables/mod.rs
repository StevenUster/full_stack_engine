//! One file per database table. These structs ARE the schema: `fse migrate`
//! diffs them against `.fse/schema.json` and writes plain sqlx migrations,
//! and the ORM's queries are generated from (and compile-time-checked
//! against) the same definitions. Change a struct, run `fse migrate`, done.

pub mod order;
pub mod product;
pub mod user;
