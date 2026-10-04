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
// Themes: every folder of themes/ + the default theme (a crate)
// ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
/// Every theme the app installs: each folder of `themes/` (built ones embed
/// their `dist/`), plus `fse-theme-default`, which `themes/starter` extends.
/// The active one is `[themes] active` in `fse.toml`; `THEME=...` overrides
/// it at boot.
pub fn themes() -> ThemeSet {
    full_stack_engine::themes!().with(Theme::embedded(&fse_theme_default::DIST))
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

/// Runs the application; `main.rs` is only a thin wrapper around this.
pub async fn run() -> std::io::Result<()> {
    app().run().await
}

/// The whole app as one builder — what [`run`] serves and what the tests
/// drive (`full_stack_engine::testing::TestApp::new(starter::app())`), so
/// both see the same routes, modules, locales and middleware.
pub fn app() -> FrameworkApp {
    FrameworkApp::new()
        .themes(themes())
        // Identity on every log line, span and error report. Both are
        // overridable by SERVICE_NAME/SERVICE_VERSION, which is how a release
        // pipeline substitutes the commit SHA for the crate version.
        .service_name(env!("CARGO_PKG_NAME"))
        .service_version(env!("CARGO_PKG_VERSION"))
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
        // `/api/openapi.json` + a browsable `/api/docs`, generated from the
        // `#[model(api)]` structs — it cannot drift from the endpoints.
        .api_docs(full_stack_engine::models::openapi::ApiDocs::new(
            "Starter Public API",
            env!("CARGO_PKG_VERSION"),
            "Read-only, unauthenticated access to the published product catalog.",
        ))
        // The API is meant to be read cross-origin. Note this applies
        // app-wide, not only to /api: `Any` never permits credentials, so a
        // caller only ever sees the anonymous page its own server could fetch.
        // `CORS_ALLOWED_ORIGINS` overrides this without a deploy.
        .cors(full_stack_engine::config::CorsConfig::Any)
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
}
