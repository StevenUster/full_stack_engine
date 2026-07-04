#![deny(warnings, unused_imports, dead_code, clippy::all, clippy::pedantic)]
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

// Locale JSON is embedded into the binary too, so there is no `locales/`
// directory to ship next to the executable.
pub static LOCALES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/locales");

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Define all roles here
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
define_roles! {
    (Admin,   "admin",   ["all"]),
    (Manager, "manager", ["users.read", "users.write"]),
    (User,    "user",    []),
    (None,    "none",    ["none"]),
}

/// Builds and runs the application; `main.rs` is only a thin wrapper around
/// this.
pub async fn run() -> std::io::Result<()> {
    FrameworkApp::new(&DIST_DIR)
        .global_context_injector(|req, value| {
            // Load locales/*.json and expose t/lang/i18n to every template.
            // Add more languages by dropping in another locales/<code>.json
            // file; nothing else needs to change.
            inject_locale_context(value, &LOCALES_DIR, "en");

            // Automatically inject user claims if a valid JWT token is present
            if let Ok(claims) = read_jwt::<AppRole>(req)
                && let Some(obj) = value.as_object_mut()
            {
                obj.insert(
                    "can_read_users".to_string(),
                    serde_json::json!(claims.role.has_permission("users.read")),
                );
                obj.insert(
                    "user".to_string(),
                    serde_json::to_value(&claims).unwrap_or(serde_json::json!({})),
                );
            }

            // EXAMPLE: Overriding or adding a custom global variable
            // if let Some(obj) = value.as_object_mut() {
            //     obj.insert("site_name".to_string(), serde_json::json!("My Awesome App"));
            // }
        })
        .configure(services::configure)
        .cronjobs(cronjobs::add_cronjobs)
        // Migrations are embedded into the binary at compile time.
        .migrator(sqlx::migrate!())
        .run()
        .await
}
