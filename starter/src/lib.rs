#![recursion_limit = "256"]
#![deny(warnings, unused_imports, dead_code, clippy::all, clippy::pedantic)]
// Long request handlers are accepted here: they are linear
// validate → query → render flows, and splitting them into pieces would hurt
// readability more than the length does.
#![allow(clippy::too_many_lines)]
// This is an application crate: the lib target exists only so integration
// tests (`tests/`) can call into the binary's code. Documentation lints for
// public library APIs therefore don't apply.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

use crate::include_dir::{Dir, include_dir};
use full_stack_engine::define_roles;
pub use full_stack_engine::prelude::*;

pub mod cronjobs;
pub mod services;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Set html template directory
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
pub static DIST_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/frontend/dist");

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Locale JSON, embedded into the binary
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
pub static LOCALES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/locales");

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Define all roles here
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
define_roles! {
    (Admin,   "admin",   ["all"]),
    (Manager, "manager", ["users.read", "users.write", "products.read", "products.write"]),
    (User,    "user",    []),
    (None,    "none",    ["none"]),
}

/// Builds and runs the application; `main.rs` is only a thin wrapper around
/// this.
pub async fn run() -> std::io::Result<()> {
    FrameworkApp::new(&DIST_DIR)
        .configure(services::configure)
        .cronjobs(cronjobs::add_cronjobs)
        // Migrations are embedded in the binary at compile time.
        .migrator(sqlx::migrate!())
        // The public JSON API is meant to be consumed by other servers/sites,
        // so it must not be caught by the site-wide per-IP limiter.
        .rate_limit_exempt_prefixes(["/api"])
        .global_context_injector(|req, value| {
            // Expose t/lang/i18n to every template from the embedded locales.
            full_stack_engine::i18n::inject_locale_context(value, &LOCALES_DIR, "en");

            // Automatically inject user claims if a valid JWT token is present
            if let Ok(claims) = read_jwt::<AppRole>(req)
                && let Some(obj) = value.as_object_mut()
            {
                obj.insert(
                    "can_read_users".to_string(),
                    serde_json::json!(claims.role.has_permission("users.read")),
                );
                obj.insert(
                    "can_read_products".to_string(),
                    serde_json::json!(claims.role.has_permission("products.read")),
                );
                obj.insert(
                    "is_admin".to_string(),
                    serde_json::json!(claims.role.is_admin()),
                );
                obj.insert(
                    "user".to_string(),
                    serde_json::to_value(&claims).unwrap_or(serde_json::json!({})),
                );
            }
        })
        .run()
        .await
}
