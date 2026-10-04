#![deny(warnings, clippy::all, clippy::pedantic)]
// An application crate: the lib target exists so tests can call `app()`.
#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

use crate::include_dir::{Dir, include_dir};
use full_stack_engine::define_roles;
pub use full_stack_engine::prelude::*;

pub mod models;
pub mod services;

/// App translations (`models.{table}.*` labels and the app's own keys),
/// layered over the framework's built-in ones.
pub static LOCALES_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/locales");

// Every role and what it may do. Generated routes check `{base}.read` for
// pages that show data and `{base}.write` for anything that changes it;
// `{base}` is the model's table name (or `permission = "..."`). "all" makes
// a role an admin.
define_roles! {
    (Admin, "admin", ["all"]),
    (User,  "user",  ["notes.read", "notes.write"]),
    (None,  "none",  ["none"]),
}

/// Every folder of `themes/` (none yet) plus the default theme. To restyle
/// pages, add a theme folder there (see AGENTS.md §8).
pub fn themes() -> ThemeSet {
    full_stack_engine::themes!().with(Theme::embedded(&fse_theme_default::DIST))
}

pub async fn run() -> std::io::Result<()> {
    app().run().await
}

/// The whole app — served by `run()` and driven by the tests
/// (`TestApp::new(app())`).
pub fn app() -> FrameworkApp {
    FrameworkApp::new()
        .themes(themes())
        .service_name(env!("CARGO_PKG_NAME"))
        .service_version(env!("CARGO_PKG_VERSION"))
        // Hand-written routes (special cases only) — they win on a path
        // conflict with anything generated.
        .configure(services::configure)
        // Login, registration, password reset, settings, user admin.
        .module(full_stack_engine::auth_module::module::<AppRole>())
        // Everything the #[model] structs in src/models/ declare.
        .models::<AppRole>()
        .locales(
            &LOCALES_DIR,
            full_stack_engine::i18n::LocaleSelector::Hardcoded("en".into()),
        )
        .migrator(sqlx::migrate!())
}
