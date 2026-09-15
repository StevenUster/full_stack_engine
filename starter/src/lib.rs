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
pub mod models;
pub mod services;

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Themes: the default theme (a crate) + this app's child theme (theme/)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
/// The built child theme (`cd theme && bun run build`). Its `theme.json`
/// names `fse-theme-default` as parent: templates and assets it doesn't
/// have come from the parent at runtime.
pub static THEME_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/theme/dist");

/// Every theme the app installs; the child (the one nothing extends) is
/// active. To run on the plain default theme instead, set `THEME=fse-theme-default`.
pub fn themes() -> Vec<Theme> {
    vec![
        Theme::embedded(&fse_theme_default::DIST),
        Theme::embedded(&THEME_DIR).dev_server("http://localhost:4321"),
    ]
}

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Locale JSON, embedded into the binary
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
pub static LOCALES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/locales");

// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
// Define all roles here
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
define_roles! {
    (Admin,   "admin",   ["all"]),
    (Manager, "manager", ["users.read", "users.write", "products.read", "products.write", "orders.read", "orders.write"]),
    (User,    "user",    []),
    (None,    "none",    ["none"]),
}

/// Builds and runs the application; `main.rs` is only a thin wrapper around
/// this.
pub async fn run() -> std::io::Result<()> {
    let mut app = FrameworkApp::new();
    for theme in themes() {
        app = app.theme(theme);
    }
    app
        // Hand-written overrides/custom flows — registered first, so they
        // beat module and generated routes on a path conflict.
        .configure(services::configure)
        // Login/registration/password-reset/settings/user-admin, complete
        // with pages and emails from the theme.
        .module(full_stack_engine::auth_module::module::<AppRole>())
        // Generated admin CRUD for every #[model] struct in src/models/.
        .models::<AppRole>()
        // App locale files layer over the framework's built-in translations;
        // pick ONE language strategy: Hardcoded, Domain or Path.
        .locales(
            &LOCALES_DIR,
            full_stack_engine::i18n::LocaleSelector::Hardcoded("en".into()),
        )
        .cronjobs(cronjobs::add_cronjobs)
        // Migrations are embedded in the binary at compile time.
        .migrator(sqlx::migrate!())
        // The public JSON API is meant to be consumed by other servers/sites,
        // so it must not be caught by the site-wide per-IP limiter.
        .rate_limit_exempt_prefixes(["/api"])
        .global_context_injector(|req, value| {
            // t/lang/i18n, `nav` (readable models) and `user` are injected
            // by the framework; this only adds the app's own extras on top.
            if let Ok(claims) = read_jwt::<AppRole>(req)
                && let Some(obj) = value.as_object_mut()
            {
                obj.insert(
                    "can_read_products".to_string(),
                    serde_json::json!(claims.role.has_permission("products.read")),
                );
            }
        })
        .run()
        .await
}
