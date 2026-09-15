#![deny(warnings, clippy::all, clippy::pedantic)]
//! The built `fse-theme-default` theme (templates, assets and `theme.json`),
//! embedded into the binary. Install it in a `full_stack_engine` app:
//!
//! ```ignore
//! FrameworkApp::new().theme(Theme::embedded(&fse_theme_default::DIST))
//! ```
//!
//! Its Astro sources are published on npm under the same name, for child
//! themes that extend it.
//!
//! This crate is built from `dist/`; run `bun run build` in this folder
//! before `cargo publish` (or before building an app against a path
//! dependency on it).

use include_dir::{Dir, include_dir};

/// The theme's build output. Its `theme.json` names it `fse-theme-default`.
pub static DIST: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/dist");
