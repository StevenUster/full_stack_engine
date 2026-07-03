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

// Locale JSON is embedded into the binary too, so there is no `locales/`
// directory to ship next to the executable.
static LOCALES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/locales");

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Define all roles here
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
define_roles! {
    (Admin,   "admin",   ["all"]),
    (Manager, "manager", ["users.read", "users.write"]),
    (User,    "user",    []),
    (None,    "none",    ["none"]),
}

#[main]
async fn main() -> std::io::Result<()> {
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
