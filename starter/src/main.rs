#![deny(warnings, unused_imports, dead_code, clippy::all, clippy::pedantic)]

use crate::include_dir::{Dir, include_dir};
pub use full_stack_engine::prelude::*;

mod cronjobs;
mod services;

static DIST_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/frontend/dist");

#[main]
async fn main() -> std::io::Result<()> {
    FrameworkApp::new(&DIST_DIR)
        .configure(services::configure)
        .cronjobs(cronjobs::add_cronjobs)
        .run()
        .await
}
