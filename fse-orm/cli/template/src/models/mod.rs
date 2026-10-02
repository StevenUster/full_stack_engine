//! THE APP. One `#[model]` struct per file (file name = `snake_case` struct
//! name); each defines its table, its pages, its permissions and its rules.
//! After changing a struct run `fse migrate`.

pub mod note;
pub mod user;
