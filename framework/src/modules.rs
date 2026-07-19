//! Reusable app modules — a module is one cargo crate that can carry
//! everything an app's `src/` carries: `#[model]` structs, hand-written
//! routes, cronjobs, locale files, and frontend sources.
//!
//! The Rust side needs nothing beyond a dependency and one builder call:
//! `#[model]` structs in the module crate register themselves through the
//! same `inventory` path as app models (linking the crate is enough — and
//! the `FrameworkApp::module` call guarantees it's linked), while routes,
//! cronjobs and locales are contributed through the [`ModuleDef`] the module
//! exposes from a `pub fn module() -> ModuleDef`.
//!
//! Layering everywhere puts the app in charge:
//! - routes: app > modules (registration order) > generated CRUD,
//! - locales: framework < modules < app (deep-merged per language),
//! - frontend pages: app > theme > modules (see the fse-ssr integration; the
//!   `fse` CLI copies a module's `frontend/` sources into `.fse/modules/`
//!   for the app's Astro build).
//!
//! Database schema: a module ships its `.fse/schema.json` snapshot inside
//! the crate; the app's `fse migrate` merges it with the app's own tables,
//! so migrations stay app-local, ordered and reviewable.

use include_dir::Dir;
use sqlx::SqlitePool;
use tokio_cron_scheduler::JobScheduler;

/// A module's async cronjob installer. A plain `fn` (not a boxed closure) so
/// a `ModuleDef` can be built in const/static contexts.
pub type ModuleCronFn = fn(
    JobScheduler,
    SqlitePool,
) -> std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(), Box<dyn std::error::Error>>>>,
>;

/// Everything one module contributes at runtime. Constructed by the module
/// crate's `pub fn module() -> ModuleDef` and handed to
/// [`crate::FrameworkApp::module`].
pub struct ModuleDef {
    /// Unique short name — also the folder the CLI extracts the module's
    /// frontend sources into (`.fse/modules/<name>/`).
    pub name: &'static str,
    /// Hand-written routes (the module's `services/`). Registered after the
    /// app's routes (app wins on conflict) and before generated CRUD (a
    /// module can override the generated endpoints of its own models).
    pub routes: Option<fn(&mut actix_web::web::ServiceConfig)>,
    /// Locale files, layered between the framework's and the app's.
    pub locales: Option<&'static Dir<'static>>,
    /// Scheduled jobs, started alongside the app's own.
    pub cronjobs: Option<ModuleCronFn>,
}

impl ModuleDef {
    #[must_use]
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            routes: None,
            locales: None,
            cronjobs: None,
        }
    }

    #[must_use]
    pub fn routes(mut self, routes: fn(&mut actix_web::web::ServiceConfig)) -> Self {
        self.routes = Some(routes);
        self
    }

    #[must_use]
    pub fn locales(mut self, locales: &'static Dir<'static>) -> Self {
        self.locales = Some(locales);
        self
    }

    #[must_use]
    pub fn cronjobs(mut self, cronjobs: ModuleCronFn) -> Self {
        self.cronjobs = Some(cronjobs);
        self
    }
}
