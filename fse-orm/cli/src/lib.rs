//! Library behind the `fse` binary, split out so the migrate flow is
//! integration-testable without spawning the binary.

pub mod config;
pub mod migrate;
