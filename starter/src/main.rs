#![deny(warnings, unused_imports, dead_code, clippy::all, clippy::pedantic)]

use crate::include_dir::{Dir, include_dir};
use full_stack_engine::define_roles;
pub use full_stack_engine::prelude::*;

mod cronjobs;
mod services;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Set html template directory
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
static DIST_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/frontend/dist");

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Define all roles here
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
define_roles! {
    (Admin,   "admin",   admin: true,  none: false, perms: []),
    (Manager, "manager", admin: false, none: false, perms: ["users.read", "users.write"]),
    (User,    "user",    admin: false, none: false, perms: []),
    (None,    "none",    admin: false, none: true,  perms: []),
}

#[main]
async fn main() -> std::io::Result<()> {
    FrameworkApp::new(&DIST_DIR)
        .configure(services::configure)
        .cronjobs(cronjobs::add_cronjobs)
        .run()
        .await
}
