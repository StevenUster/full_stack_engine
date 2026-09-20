#![deny(warnings, unused_imports, dead_code, clippy::all, clippy::pedantic)]

use actix_web::{
    App, HttpMessage, HttpResponse, HttpServer,
    body::MessageBody,
    dev::{Service as _, ServiceResponse},
    http::StatusCode,
    middleware::{DefaultHeaders, ErrorHandlerResponse, ErrorHandlers, NormalizePath},
    web,
};
use dotenvy::dotenv;
use include_dir::Dir;
use sqlx::sqlite::SqlitePool;
use std::{env, fs};
use tera::{Context, Tera};
use tokio_cron_scheduler::JobScheduler;
use tracing::{debug, error, info};
use tracing_actix_web::{RequestId, TracingLogger};

pub mod auth;
pub mod auth_module;
pub mod config;
pub mod cron;
pub mod error;
pub mod i18n;
pub mod mail;
pub mod models;
pub mod modules;
pub mod observability;
pub mod prelude;
pub mod rate_limiter;
pub mod roles;
pub mod structs;
pub mod testing;
pub mod themes;
pub mod uploads;

// Re-exported because the code emitted by `#[derive(Model)]` submits its
// registration through `::full_stack_engine::inventory::submit!` — apps never
// need their own `inventory` dependency.
pub use inventory;

pub type ContextInjectorFn =
    Box<dyn Fn(&actix_web::HttpRequest, &mut serde_json::Value) + Send + Sync + 'static>;

#[derive(Copy, Clone, PartialEq, serde::Serialize)]
pub enum Env {
    Dev,
    Prod,
}

pub struct AppData {
    pub tera: Tera,
    pub db: SqlitePool,
    /// Convenience mirrors of the matching [`config::Config`] fields, which is
    /// the source of truth. Kept because handlers read them constantly and
    /// they are immutable for the process's lifetime.
    pub env: Env,
    pub domain: String,
    pub protocol: String,
    pub smtp_from: String,
    pub email_verification_enabled: bool,
    pub context_injector: Option<std::sync::Arc<ContextInjectorFn>>,
    /// Every language the app serves, already resolved: framework base
    /// translations < app files, and each non-default language deep-merged
    /// over the default (see [`i18n::build_locales`]/[`i18n::resolve_locales`]).
    pub locales: std::collections::HashMap<String, serde_json::Value>,
    /// How a request's language is decided (see [`FrameworkApp::locales`]).
    pub locale_selector: i18n::LocaleSelector,
    /// The active theme and its ancestors (see [`themes`]). `tera` holds the
    /// stack's templates; this is kept for static assets and dev servers.
    pub themes: std::sync::Arc<themes::ThemeStack>,
    /// The whole validated configuration, including the secrets (which are
    /// [`secrecy::SecretString`] and so cannot be logged by accident).
    pub config: std::sync::Arc<config::Config>,
}

impl AppData {
    /// The JWT signing key.
    ///
    /// A method rather than a field so every read of the secret is a visible
    /// call rather than a field access that could be swept into a `Debug`
    /// print or a serialised context.
    #[must_use]
    pub fn jwt_secret(&self) -> &str {
        self.config.jwt_secret()
    }
}

impl AppData {
    /// Everything the framework puts into every page's context before the
    /// app's own injector runs: `t`/`lang`/`lang_prefix` and an
    /// (empty unless [`FrameworkApp::models`] fills it) `nav` list, so theme
    /// layouts can always loop over it.
    pub fn inject_request_context(
        &self,
        req: &actix_web::HttpRequest,
        value: &mut serde_json::Value,
    ) {
        self.inject_request_locale(req, value);
        if let Some(obj) = value.as_object_mut() {
            obj.entry("nav").or_insert_with(|| serde_json::json!([]));
        }
    }

    /// The language resolved for this request (set by the framework's locale
    /// middleware), falling back to the selector's default.
    #[must_use]
    pub fn request_lang(&self, req: &actix_web::HttpRequest) -> String {
        req.extensions().get::<i18n::RequestLang>().map_or_else(
            || self.locale_selector.default_lang().to_string(),
            |l| l.0.clone(),
        )
    }

    /// The URL prefix links must carry to stay in the request's language:
    /// `"/de"` in `Path` mode on a non-default language, otherwise `""`.
    #[must_use]
    pub fn lang_prefix(&self, req: &actix_web::HttpRequest) -> String {
        if let i18n::LocaleSelector::Path { default } = &self.locale_selector {
            let lang = self.request_lang(req);
            if lang != *default {
                return format!("/{lang}");
            }
        }
        String::new()
    }

    /// One language's full translation tree (already fallback-resolved).
    /// Unknown languages get the default language's tree.
    #[must_use]
    pub fn locale(&self, lang: &str) -> serde_json::Value {
        self.locales
            .get(lang)
            .or_else(|| self.locales.get(self.locale_selector.default_lang()))
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}))
    }

    /// Inserts `t`, `lang` and `lang_prefix` for the request's resolved
    /// language — runs automatically before every `render_tpl`.
    ///
    /// `t` is **one** language's tree, not all of them. Every page's render
    /// context is also serialised into the page for client-side code (see
    /// `inject_page_props`), and this used to insert an `i18n` key holding
    /// *every* configured language's full translations: 16 KB per page in the
    /// starter, 40 KB in a real app, referenced by no template and no client
    /// script. Apps that genuinely need another language on the client should
    /// fetch it, not ship it on every page.
    pub fn inject_request_locale(
        &self,
        req: &actix_web::HttpRequest,
        value: &mut serde_json::Value,
    ) {
        let Some(obj) = value.as_object_mut() else {
            return;
        };
        let lang = self.request_lang(req);
        obj.insert("t".to_string(), self.locale(&lang));
        obj.insert(
            "lang_prefix".to_string(),
            serde_json::json!(self.lang_prefix(req)),
        );
        obj.insert("lang".to_string(), serde_json::json!(lang));
    }
}

pub trait RenderTplExt {
    fn render_tpl<'a, T: serde::Serialize>(
        &'a self,
        template: &'a str,
        context: &T,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HttpResponse> + 'a>>;
}

impl RenderTplExt for actix_web::HttpRequest {
    fn render_tpl<'a, T: serde::Serialize>(
        &'a self,
        template: &'a str,
        context: &T,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = HttpResponse> + 'a>> {
        let app_data = self
            .app_data::<actix_web::web::Data<crate::AppData>>()
            .unwrap()
            .clone();
        let mut value = serde_json::to_value(context).unwrap_or_else(|_| serde_json::json!({}));

        // Locale context first, app injector second — an app that wants to
        // override `t`/`lang` for a request simply wins.
        app_data.inject_request_context(self, &mut value);
        if let Some(injector) = &app_data.context_injector {
            injector(self, &mut value);
        }

        let template_owned = template.to_string();
        Box::pin(async move { app_data.render_template(&template_owned, &value).await })
    }
}

impl AppData {
    pub async fn render(&self, template: &str) -> HttpResponse {
        self.render_template(template, &serde_json::json!({})).await
    }

    pub async fn render_tpl<T: serde::Serialize>(
        &self,
        template: &str,
        context: &T,
    ) -> HttpResponse {
        self.render_template(template, context).await
    }

    pub async fn render_template<T: serde::Serialize>(
        &self,
        template_name: &str,
        context_data: &T,
    ) -> HttpResponse {
        let value = match serde_json::to_value(context_data) {
            Ok(value) => value,
            Err(err) => {
                error!("Context serialization error: {err}");
                return HttpResponse::InternalServerError().body("Context serialization error");
            }
        };
        let context = match Context::from_serialize(&value) {
            Ok(ctx) => ctx,
            Err(err) => {
                error!("Context serialization error: {err}");
                return HttpResponse::InternalServerError().finish();
            }
        };

        // In dev, a theme's dev server (child first) serves the freshest
        // version of the page; the built templates are the fallback.
        let rendered = match self.dev_template(template_name).await {
            Some(tera) => tera.render(template_name, &context),
            // Names are used verbatim: template names can legitimately
            // contain underscores (e.g. a `sort_items` table's pages).
            None => self.tera.render(template_name, &context),
        };
        match rendered {
            Ok(html) => HttpResponse::Ok()
                .content_type("text/html")
                .body(inject_page_props(html, &value)),
            Err(err) => {
                error!("Template rendering error ({template_name}): {err}");
                HttpResponse::InternalServerError().finish()
            }
        }
    }

    /// `ENV=dev` only: the page fetched from the first theme dev server that
    /// serves it, parsed into a copy of the built templates (so it can still
    /// extend/include them). `None` in prod, or when no dev server has it.
    async fn dev_template(&self, template_name: &str) -> Option<Tera> {
        if self.env != Env::Dev {
            return None;
        }
        let path = if template_name == "index" {
            ""
        } else {
            template_name
        };
        for server in self.themes.dev_servers() {
            let url = format!("{server}/{path}");
            let html = match reqwest::get(&url).await {
                Ok(res) if res.status().is_success() => match res.text().await {
                    Ok(html) => html,
                    Err(err) => {
                        error!("Failed to read {url}: {err}");
                        continue;
                    }
                },
                Ok(res) => {
                    debug!("Theme dev server {url} returned {}", res.status());
                    continue;
                }
                Err(err) => {
                    debug!("Theme dev server {server} unreachable: {err}");
                    continue;
                }
            };
            let mut tera = self.tera.clone();
            if let Err(err) = tera.add_raw_template(template_name, &html) {
                error!("Dev template {template_name} from {server} is invalid: {err}");
                return None;
            }
            return Some(tera);
        }
        None
    }

    /// Renders an email template to an HTML string (via the Astro dev server
    /// in dev, from the embedded templates in prod).
    ///
    /// # Errors
    ///
    /// Returns a [`RenderError`] — a real error type rather than a `String`, so
    /// a caller can attach context with
    /// [`ErrorContext::context`](crate::error::ErrorContext::context) and keep
    /// Tera's own explanation reachable through `source()`.
    pub async fn render_email<T: serde::Serialize>(
        &self,
        template_name: &str,
        context_data: &T,
    ) -> Result<String, RenderError> {
        let context = Context::from_serialize(context_data).map_err(RenderError::Context)?;
        let rendered = match self.dev_template(template_name).await {
            Some(tera) => tera.render(template_name, &context),
            None => self.tera.render(template_name, &context),
        };
        rendered.map_err(|source| RenderError::Template {
            template: template_name.to_string(),
            source,
        })
    }
}

/// Why a template could not be turned into HTML.
///
/// Tera's own message stays reachable as `source()`, so
/// [`crate::observability::source_chain`] renders the whole explanation —
/// which for a template error is the part that names the line and the missing
/// variable.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("serializing the template context")]
    Context(#[source] tera::Error),
    #[error("rendering template `{template}`")]
    Template {
        template: String,
        #[source]
        source: tera::Error,
    },
}

/// Placeholder emitted by the frontend layout. When present, it is filled
/// with the page's render context as JSON so client-side code (islands,
/// inline scripts) can read the same data the page was rendered with —
/// without a second request. Pages whose layout omits the tag get nothing
/// injected.
const PAGE_PROPS_TAG: &str = r#"<script type="application/json" id="__fse-props__"></script>"#;

fn inject_page_props(html: String, context: &serde_json::Value) -> String {
    if !html.contains(PAGE_PROPS_TAG) {
        return html;
    }
    let Ok(json) = serde_json::to_string(context) else {
        return html;
    };
    // Escape `<` so context data containing "</script>" (or "<!--") cannot
    // break out of the script element; `<` is valid JSON and decodes
    // back to `<` in `JSON.parse`.
    let json = json.replace('<', "\\u003c");
    let filled = format!(r#"<script type="application/json" id="__fse-props__">{json}</script>"#);
    html.replacen(PAGE_PROPS_TAG, &filled, 1)
}

type ConfigureFn = Box<dyn Fn(&mut web::ServiceConfig) + Send + Sync + 'static>;
type CronjobsFn = Box<
    dyn FnOnce(
        JobScheduler,
        SqlitePool,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), Box<dyn std::error::Error>>>>,
    >,
>;
type StartupFn = Box<
    dyn FnOnce(
        SqlitePool,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), Box<dyn std::error::Error>>>>,
    >,
>;

/// The generated-CRUD mounter installed by [`FrameworkApp::models`] —
/// shared (`Arc`) because every worker's `App` applies it.
type ModelRoutesFn = std::sync::Arc<dyn Fn(&mut web::ServiceConfig) + Send + Sync>;

pub struct FrameworkApp {
    themes: Vec<themes::Theme>,
    active_theme: Option<String>,
    nav_injector: Option<std::sync::Arc<ContextInjectorFn>>,
    configure_fn: Option<ConfigureFn>,
    model_routes: Option<ModelRoutesFn>,
    cronjobs_fn: Option<CronjobsFn>,
    startup_fn: Option<StartupFn>,
    context_injector: Option<std::sync::Arc<ContextInjectorFn>>,
    migrator: Option<sqlx::migrate::Migrator>,
    rate_limit_exempt_prefixes: Vec<String>,
    locales_dir: Option<&'static Dir<'static>>,
    locale_selector: i18n::LocaleSelector,
    modules: Vec<modules::ModuleDef>,
    service_name: Option<String>,
    service_version: Option<String>,
}

impl Default for FrameworkApp {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameworkApp {
    #[must_use]
    pub fn new() -> Self {
        Self {
            themes: Vec::new(),
            active_theme: None,
            nav_injector: None,
            configure_fn: None,
            model_routes: None,
            cronjobs_fn: None,
            startup_fn: None,
            context_injector: None,
            migrator: None,
            rate_limit_exempt_prefixes: Vec::new(),
            locales_dir: None,
            locale_selector: i18n::LocaleSelector::default(),
            modules: Vec::new(),
            service_name: None,
            service_version: None,
        }
    }

    /// The name this app reports as `service.name` on every span and log
    /// record — what a telemetry backend groups by, and what tells two
    /// services apart in one trace.
    ///
    /// Defaults to the `SERVICE_NAME` environment variable, then to the
    /// executable's file name. Pass `env!("CARGO_PKG_NAME")` to pin it to the
    /// crate name instead:
    ///
    /// ```ignore
    /// FrameworkApp::new()
    ///     .service_name(env!("CARGO_PKG_NAME"))
    ///     .service_version(env!("CARGO_PKG_VERSION"))
    /// ```
    #[must_use]
    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = Some(name.into());
        self
    }

    /// The release this app reports as `service.version` — the thing that lets
    /// an error backend say "started in v1.4.2" and a dashboard compare error
    /// rates across deploys.
    ///
    /// Defaults to the `SERVICE_VERSION` environment variable, then to
    /// `"unknown"`. In CI, prefer the commit SHA over the crate version, since
    /// two builds of the same version are not the same binary.
    #[must_use]
    pub fn service_version(mut self, version: impl Into<String>) -> Self {
        self.service_version = Some(version.into());
        self
    }

    /// Installs a theme (see [`themes`]). Install a parent and its child and
    /// the child becomes active; the parent fills in every template and
    /// asset the child doesn't have. Several unrelated themes can be
    /// installed side by side — pick one with [`FrameworkApp::active_theme`]
    /// or the `THEME` environment variable.
    ///
    /// ```ignore
    /// static THEME: Dir = include_dir!("$CARGO_MANIFEST_DIR/theme/dist");
    ///
    /// FrameworkApp::new()
    ///     .theme(Theme::embedded(&fse_theme_default::DIST))
    ///     .theme(Theme::embedded(&THEME).dev_server("http://localhost:4321"))
    /// ```
    #[must_use]
    pub fn theme(mut self, theme: themes::Theme) -> Self {
        self.themes.push(theme);
        self
    }

    /// Activates an installed theme by its `theme.json` name. The `THEME`
    /// environment variable, when set, takes precedence.
    #[must_use]
    pub fn active_theme(mut self, name: impl Into<String>) -> Self {
        self.active_theme = Some(name.into());
        self
    }

    /// Adds a reusable module (see [`modules::ModuleDef`]): its routes mount
    /// between the app's and the generated CRUD, its locales layer between
    /// the framework's and the app's, its cronjobs start with the app's, and
    /// its `#[model]` structs register simply because the crate is linked.
    ///
    /// ```ignore
    /// FrameworkApp::new()
    ///     .module(fse_module_erp::module())
    /// ```
    #[must_use]
    pub fn module(mut self, def: modules::ModuleDef) -> Self {
        self.modules.push(def);
        self
    }

    /// The app's locale files and the one language-switching strategy in
    /// effect. Every rendered template automatically receives `t` (the
    /// request's language, framework base translations < app files, missing
    /// keys falling back to the default language), `lang`, `lang_prefix`
    /// and `i18n`.
    ///
    /// ```ignore
    /// static LOCALES: Dir = include_dir!("$CARGO_MANIFEST_DIR/locales");
    ///
    /// // one fixed language:
    /// .locales(&LOCALES, LocaleSelector::Hardcoded("en".into()))
    /// // by domain:
    /// .locales(&LOCALES, LocaleSelector::Domain {
    ///     map: vec![("example.de".into(), "de".into())],
    ///     default: "en".into(),
    /// })
    /// // by path prefix (default language unprefixed, /de/... for German):
    /// .locales(&LOCALES, LocaleSelector::Path { default: "en".into() })
    /// ```
    #[must_use]
    pub fn locales(mut self, dir: &'static Dir<'static>, selector: i18n::LocaleSelector) -> Self {
        self.locales_dir = Some(dir);
        self.locale_selector = selector;
        self
    }

    /// Mounts generated CRUD routes for every `#[model]` struct linked into
    /// the binary (including ones from module crates). `R` is the app's role
    /// enum from `define_roles!` — generated endpoints check the model's
    /// conventional `<base>.read`/`<base>.write` permissions against it.
    ///
    /// Generated routes register *after* [`FrameworkApp::configure`] routes,
    /// so a same-path route in the app simply overrides the generated one.
    ///
    /// ```ignore
    /// FrameworkApp::new()
    ///     .configure(services::configure)
    ///     .models::<AppRole>()
    ///     .run().await
    /// ```
    #[must_use]
    pub fn models<R: structs::Role>(mut self) -> Self {
        self.model_routes = Some(std::sync::Arc::new(models::mount_all::<R>));
        self.nav_injector = Some(std::sync::Arc::new(Box::new(models::inject_nav::<R>)));
        self
    }

    /// Path prefixes exempt from the site-wide rate limiter (see
    /// [`rate_limiter::global_rate_limiter`]). Use for routes that are hit
    /// legitimately from a single IP at high volume — e.g. a public `/api`
    /// consumed server-side by an SSR site, which would otherwise be throttled
    /// as one client. An exempt prefix has **no** per-IP limit, so keep the list
    /// tight.
    ///
    /// ```ignore
    /// FrameworkApp::new().rate_limit_exempt_prefixes(["/api"]).run().await
    /// ```
    #[must_use]
    pub fn rate_limit_exempt_prefixes<I, S>(mut self, prefixes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.rate_limit_exempt_prefixes = prefixes.into_iter().map(Into::into).collect();
        self
    }

    /// Supplies migrations embedded in the binary at compile time, so a built
    /// app carries its schema with it and needs no `migrations/` directory next
    /// to the executable. Pass `sqlx::migrate!()` (which reads
    /// `$CARGO_MANIFEST_DIR/migrations`) from your app:
    ///
    /// ```ignore
    /// FrameworkApp::new().migrator(sqlx::migrate!()).run().await
    /// ```
    ///
    /// When omitted, migrations are loaded at runtime from the `MIGRATIONS_DIR`
    /// environment variable (default `./migrations`).
    #[must_use]
    pub fn migrator(mut self, migrator: sqlx::migrate::Migrator) -> Self {
        self.migrator = Some(migrator);
        self
    }

    #[must_use]
    pub fn global_context_injector<F>(mut self, f: F) -> Self
    where
        F: Fn(&actix_web::HttpRequest, &mut serde_json::Value) + Send + Sync + 'static,
    {
        self.context_injector = Some(std::sync::Arc::new(Box::new(f)));
        self
    }

    #[must_use]
    pub fn configure<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut web::ServiceConfig) + Send + Sync + 'static,
    {
        self.configure_fn = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn cronjobs<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce(JobScheduler, SqlitePool) -> Fut + 'static,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>> + 'static,
    {
        self.cronjobs_fn = Some(Box::new(move |sched, pool| Box::pin(f(sched, pool))));
        self
    }

    /// Runs once at boot, after migrations have applied and before the HTTP
    /// server starts accepting connections — for one-time setup that needs
    /// the real database (e.g. seeding a first admin account from an env
    /// var if none exists yet). Given the same pool the app serves requests
    /// with, not a separate connection like [`FrameworkApp::cronjobs`].
    ///
    /// ```ignore
    /// FrameworkApp::new().on_startup(bootstrap_admin).run().await
    /// ```
    #[must_use]
    pub fn on_startup<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce(SqlitePool) -> Fut + 'static,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>> + 'static,
    {
        self.startup_fn = Some(Box::new(move |pool| Box::pin(f(pool))));
        self
    }

    /// Boots the application: connects the database, runs migrations, starts
    /// the cron scheduler and serves HTTP until the process is stopped.
    ///
    /// # Errors
    ///
    /// Returns [`std::io::Error`] if the database file can't be created or the
    /// server can't bind its port.
    ///
    /// # Panics
    ///
    /// Panics at startup if required configuration (`DOMAIN`, `PROTOCOL`,
    /// `JWT_SECRET`, `DATABASE_URL`) is missing, or if the database, its
    /// migrations, the [`FrameworkApp::on_startup`] hook, or the cron
    /// scheduler fail to initialize — failing loudly at boot instead of
    /// running half-configured.
    // The boot sequence reads top-to-bottom on purpose; splitting it into
    // helpers would only scatter the order things happen in.
    #[allow(clippy::too_many_lines)]
    pub async fn run(mut self) -> std::io::Result<()> {
        // The `.env` file is read *before* observability is configured, or
        // `RUST_LOG`/`LOG_FORMAT`/`SENTRY_DSN` set there would be invisible —
        // the logger used to be initialised first, which silently ignored
        // every logging setting an app kept in its `.env`.
        let env_file = load_env_file();

        // `--healthcheck` turns the binary into its own probe and exits. This
        // exists so a container image needs no `curl`: the runtime layer is
        // `debian-slim` plus ca-certificates, and adding an HTTP client to it
        // just to answer HEALTHCHECK would be a package (and its CVE stream)
        // carried solely for that.
        if env::args().skip(1).any(|arg| arg == "--healthcheck") {
            return run_healthcheck().await;
        }

        let env = config::parse_env(env::var("ENV").ok().as_deref());

        // The builder's identity is a *default*: `SERVICE_NAME`/`SERVICE_VERSION`
        // from the environment win, so a release pipeline can stamp the commit
        // SHA without the app editing code.
        let telemetry = observability::Settings::from_env_with_defaults(
            env,
            self.service_name.take(),
            self.service_version.take(),
        );
        // Held until `run` returns, so the exporter's last batch is flushed on
        // shutdown instead of being dropped with the process.
        let _telemetry_guard = observability::init(&telemetry);

        // Reported here rather than where it happened: logging did not exist
        // yet at that point.
        if let Some(path) = env_file {
            debug!(".env file loaded from: {}", path.display());
        } else {
            debug!("No .env file found, relying on system environment variables.");
        }

        info!("Starting application...");

        // One validated read of the whole environment. Every problem is
        // reported together, so a fresh deployment is not fixed by rebooting
        // once per missing variable (see `config`).
        let cfg = std::sync::Arc::new(config::Config::from_env().unwrap_or_else(|err| {
            // `error!` first so the failure reaches the log pipeline (and
            // Sentry) in the same shape as any other, then panic to stop the
            // boot.
            error!("{err}");
            panic!("{err}");
        }));

        let db_pool = init_db(&cfg, self.migrator.take()).await?;

        if let Some(startup_fn) = self.startup_fn.take() {
            startup_fn(db_pool.clone())
                .await
                .expect("Failed to run on_startup hook");
        }

        let active_theme = cfg.theme.clone().or_else(|| self.active_theme.take());
        let theme_stack =
            themes::ThemeStack::resolve(std::mem::take(&mut self.themes), active_theme.as_deref())
                .unwrap_or_else(|err| panic!("Theme setup failed: {err}"));
        info!(
            "Theme: {}",
            theme_stack
                .chain()
                .iter()
                .map(themes::Theme::name)
                .collect::<Vec<_>>()
                .join(" -> ")
        );
        // Template names never carry a `.html` suffix, so Tera's default
        // suffix-based autoescape detection never matches; `ThemeStack::tera`
        // enables escaping for every template (raw HTML uses the `safe`
        // filter), and logs and skips broken templates.
        let tera = theme_stack.tera();
        let theme_stack = std::sync::Arc::new(theme_stack);

        let module_crons: Vec<modules::ModuleCronFn> =
            self.modules.iter().filter_map(|m| m.cronjobs).collect();
        start_cron_scheduler(self.cronjobs_fn.take(), module_crons, &cfg.database_url).await;

        let configure_fn = self.configure_fn.map(std::sync::Arc::new);
        let model_routes = self.model_routes.clone();
        // Framework-provided context (nav/user) first, the app's injector
        // second — the app can override either.
        let context_injector: Option<std::sync::Arc<ContextInjectorFn>> =
            match (self.nav_injector.clone(), self.context_injector.clone()) {
                (Some(nav), Some(app)) => Some(std::sync::Arc::new(Box::new(
                    move |req: &actix_web::HttpRequest, value: &mut serde_json::Value| {
                        nav(req, value);
                        app(req, value);
                    },
                ))),
                (nav, app) => nav.or(app),
            };

        let locale_selector = self.locale_selector.clone();
        let module_locale_dirs: Vec<&Dir> = self.modules.iter().filter_map(|m| m.locales).collect();
        let locales = i18n::resolve_locales(
            i18n::build_locales(&module_locale_dirs, self.locales_dir),
            locale_selector.default_lang(),
        );
        let known_langs: Vec<String> = locales.keys().cloned().collect();
        let module_routes: Vec<fn(&mut web::ServiceConfig)> =
            self.modules.iter().filter_map(|m| m.routes).collect();

        // Built once and shared across all workers (the config holds an Arc to
        // the token buckets), so the site-wide per-IP limit is enforced for the
        // whole process rather than per worker thread. Configured exempt
        // prefixes (e.g. a public `/api`) skip the limiter entirely.
        let global_rate_config =
            rate_limiter::global_rate_limiter(&self.rate_limit_exempt_prefixes, cfg.rate_limit);

        HttpServer::new(move || {
            let request_selector = locale_selector.clone();
            let request_known_langs = known_langs.clone();
            let mut app = App::new()
                .app_data(web::Data::new(AppData {
                    tera: tera.clone(),
                    db: db_pool.clone(),
                    env: cfg.env,
                    domain: cfg.domain.clone(),
                    protocol: cfg.protocol.clone(),
                    smtp_from: cfg.mail_from().to_string(),
                    email_verification_enabled: cfg.email_verification_enabled,
                    config: cfg.clone(),
                    context_injector: context_injector.clone(),
                    locales: locales.clone(),
                    locale_selector: locale_selector.clone(),
                    themes: theme_stack.clone(),
                }))
                // Resolve the request language (and in Path mode strip a
                // /{lang} prefix) before any routing happens.
                .wrap_fn(move |mut req, srv| {
                    i18n::apply_request_locale(&request_selector, &request_known_langs, &mut req);
                    srv.call(req)
                })
                .wrap(NormalizePath::trim())
                .wrap(
                    ErrorHandlers::new()
                        .handler(StatusCode::INTERNAL_SERVER_ERROR, render_error_page)
                        .handler(StatusCode::NOT_FOUND, render_error_page)
                        .handler(StatusCode::BAD_REQUEST, render_error_page)
                        .handler(StatusCode::UNAUTHORIZED, render_error_page)
                        .handler(StatusCode::FORBIDDEN, render_error_page),
                )
                // Mints this request's script nonce and attaches the matching
                // Content-Security-Policy to whatever comes back. Outside
                // `ErrorHandlers`, so the themed error page is covered by the
                // same policy and carries the same nonce.
                .wrap_fn(move |req, srv| {
                    let nonce = ScriptNonce::generate();
                    req.extensions_mut().insert(nonce.clone());
                    let fut = srv.call(req);
                    async move { apply_csp(env, nonce, fut.await?).await }
                })
                // Echoes the request's correlation id back to the caller, so a
                // user reporting "it broke" can quote the id that finds the
                // exact request in the logs. Registered *outside*
                // `ErrorHandlers`, because the error page is a fresh response
                // and would drop a header set beneath it.
                .wrap_fn(|req, srv| {
                    let request_id = req.extensions().get::<RequestId>().copied();
                    let fut = srv.call(req);
                    async move {
                        let mut res = fut.await?;
                        if let Some(request_id) = request_id
                            && let Ok(value) = request_id.to_string().parse()
                        {
                            res.headers_mut().insert(
                                actix_web::http::header::HeaderName::from_static("x-request-id"),
                                value,
                            );
                        }
                        Ok(res)
                    }
                })
                // One span per request, entered for the whole response
                // including the body stream — so every log line inside a
                // handler carries its `request_id`, `http.route` and (under
                // the `otel` feature) `trace_id` without the handler doing
                // anything. See `observability::FseRootSpan` for the fields
                // and for what is deliberately never recorded.
                .wrap(TracingLogger::<observability::FseRootSpan>::new())
                // Compresses whatever the stack produced — pages, JSON, theme
                // assets, error pages. Registered outside `ErrorHandlers` so
                // the rendered error page is compressed too. Nothing was
                // compressed before this: a 77 KB page went out as 77 KB even
                // when the client asked for gzip.
                .wrap(actix_web::middleware::Compress::default())
                .wrap(security_headers())
                // Outermost layer: reject per-IP floods before any routing or
                // request processing happens. Shared buckets across workers.
                .wrap(global_rate_config.clone());

            // Liveness/readiness probes, registered first so they exist even
            // if an app registers nothing. An app can still override either by
            // claiming the same path in `configure`, since actix matches in
            // registration order — but these come first precisely so a
            // deployment always has something to probe.
            app = app.configure(health_routes);

            if let Some(ref configure_fn) = configure_fn {
                let cf = configure_fn.clone();
                app = app.configure(move |cfg| (cf)(cfg));
            }

            // Module routes mount after the app's (app wins on a path
            // conflict) and before generated CRUD (a module can override the
            // generated endpoints of its own models).
            for module_cfg in &module_routes {
                app = app.configure(*module_cfg);
            }

            // Generated model CRUD (see `FrameworkApp::models`). Mounted
            // after the app's own routes, so on a path conflict the
            // hand-written route wins — that's the override mechanism.
            if let Some(ref model_routes) = model_routes {
                let mr = model_routes.clone();
                app = app.configure(move |cfg| (mr)(cfg));
            }

            // Public uploaded files (see `uploads::save_upload`, which returns
            // `/uploads/...` paths). Registered after the app's own routes so
            // an app route wins on conflict. `actix-files` rejects path
            // traversal, and only this directory is exposed — private files
            // (`data/`) stay unreachable. Created up front so a fresh
            // checkout/container without any uploads yet doesn't log an
            // `actix_files` error on every worker at boot.
            let _ = std::fs::create_dir_all("./uploads");
            // Everything no route claimed: theme assets, child theme first
            // (in dev, the themes' dev servers get the first try).
            let assets = theme_stack.clone();
            app.service(uploads_service()).default_service(web::to(
                move |req: actix_web::HttpRequest| {
                    let assets = assets.clone();
                    async move {
                        if env == Env::Dev {
                            for server in assets.dev_servers() {
                                if let Ok(res) = forward_to_dev_server(server, &req).await {
                                    return Ok::<HttpResponse, actix_web::Error>(res);
                                }
                            }
                        }
                        let path = req.path().trim_start_matches('/');
                        Ok(serve_asset(&assets, path, req.method().as_str())
                            .unwrap_or_else(|_| HttpResponse::NotFound().finish()))
                    }
                },
            ))
        })
        .bind(format!(
            "0.0.0.0:{}",
            env::var("PORT").unwrap_or_else(|_| "8080".to_string())
        ))?
        .run()
        .await
    }
}

/// Creates the `SQLite` database file if needed, connects the pool, runs
/// migrations (embedded ones when supplied, otherwise loaded from
/// `MIGRATIONS_DIR`, default `./migrations`) and sets the connection pragmas.
///
/// Panics on any database failure: booting without a working, migrated
/// database would only fail later on the first request.
async fn init_db(
    cfg: &config::Config,
    embedded_migrator: Option<sqlx::migrate::Migrator>,
) -> std::io::Result<SqlitePool> {
    let database_url = cfg.database_url.as_str();
    let db_file = database_url.trim_start_matches("sqlite:");

    if let Some(dir) = std::path::Path::new(db_file).parent() {
        fs::create_dir_all(dir)?;
    }

    if !std::path::Path::new(db_file).exists() {
        fs::File::create(db_file)?;
    }

    // Pragmas belong on the *connect options*, not on one connection out of
    // the pool: `foreign_keys` and `synchronous` are per-connection settings in
    // SQLite, so running them once against the pool would configure whichever
    // connection answered and leave the other nine alone. (sqlx already
    // defaults `foreign_keys` on and a 5s busy timeout; both are set here
    // explicitly so the guarantee is visible rather than inherited.)
    //
    // `synchronous = NORMAL` is the standard pairing with WAL: fsync happens at
    // checkpoints instead of every commit. WAL keeps the database consistent
    // across a process crash; only a host power loss can cost the last
    // transactions, which is the accepted trade for an order-of-magnitude
    // faster write path.
    let connect_options = database_url
        .parse::<sqlx::sqlite::SqliteConnectOptions>()
        .expect("Failed to parse DATABASE_URL")
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(5))
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .synchronous(sqlx::sqlite::SqliteSynchronous::Normal);

    let db_pool = sqlx::sqlite::SqlitePoolOptions::new()
        .connect_with(connect_options)
        .await
        .expect("Failed to create database pool");

    // Prefer migrations embedded in the binary (via `.migrator(...)`); fall
    // back to reading them from disk at runtime when none were supplied.
    let migrator = if let Some(migrator) = embedded_migrator {
        migrator
    } else {
        sqlx::migrate::Migrator::new(std::path::Path::new(&cfg.migrations_dir))
            .await
            .expect("Failed to load migrations")
    };
    // Migrations run on a dedicated connection with foreign-key enforcement
    // OFF: fse's table-rebuild migrations DROP the old table, and with
    // enforcement on that DROP fires child tables' ON DELETE actions (CASCADE
    // silently wipes their rows, RESTRICT fails the deploy). sqlx wraps each
    // migration in a transaction, where `PRAGMA foreign_keys` is a silent
    // no-op, so it must be a connection-level setting — the app pool keeps
    // enforcement on as before.
    {
        use sqlx::{ConnectOptions, Connection};
        let mut conn = database_url
            .parse::<sqlx::sqlite::SqliteConnectOptions>()
            .expect("Failed to parse database url")
            .foreign_keys(false)
            .connect()
            .await
            .expect("Failed to open migration connection");
        migrator
            .run(&mut conn)
            .await
            .expect("Failed to run database migrations");
        let violations: Vec<(String, String)> =
            sqlx::query_as("SELECT \"table\", parent FROM pragma_foreign_key_check")
                .fetch_all(&mut conn)
                .await
                .expect("Failed to run foreign_key_check");
        if !violations.is_empty() {
            eprintln!(
                "WARNING: database has {} foreign key violation(s) after migrations \
                 (first: table `{}` references missing `{}` row)",
                violations.len(),
                violations[0].0,
                violations[0].1
            );
        }
        conn.close().await.ok();
    }

    Ok(db_pool)
}

/// Registers the app's cron jobs (on their own DB pool) and starts the
/// scheduler if any job was added. Panics on failure — see [`FrameworkApp::run`].
async fn start_cron_scheduler(
    cronjobs_fn: Option<CronjobsFn>,
    module_crons: Vec<modules::ModuleCronFn>,
    database_url: &str,
) {
    let mut sched = JobScheduler::new()
        .await
        .expect("Failed to create job scheduler");

    if cronjobs_fn.is_some() || !module_crons.is_empty() {
        let cron_db_pool = SqlitePool::connect(database_url)
            .await
            .expect("Failed to create cron database pool");

        // Modules first, app second — purely cosmetic for job ordering; every
        // job carries its own schedule.
        for module_cron in module_crons {
            (module_cron)(sched.clone(), cron_db_pool.clone())
                .await
                .expect("Failed to add module cron jobs");
        }
        if let Some(cronjobs_fn) = cronjobs_fn {
            (cronjobs_fn)(sched.clone(), cron_db_pool)
                .await
                .expect("Failed to add cron jobs");
        }
    }

    let has_jobs = sched
        .time_till_next_job()
        .await
        .expect("Failed to check for jobs")
        .is_some();

    if has_jobs {
        sched.start().await.expect("Failed to start cron scheduler");
        info!("Cron scheduler started.");
    } else {
        info!("No cronjobs. Cron scheduler not started.");
    }
}

/// Hardened response headers applied to every response. The Content-Security-
/// Policy is **not** here: it carries a per-request nonce in production, so it
/// is built per response by [`content_security_policy`].
fn security_headers() -> DefaultHeaders {
    DefaultHeaders::new()
        .add(("X-Content-Type-Options", "nosniff"))
        .add(("X-Frame-Options", "DENY"))
        .add(("Referrer-Policy", "strict-origin-when-cross-origin"))
}

/// A per-request script nonce, put in the request's extensions so the renderer
/// can stamp it onto every `<script>` tag of the page it is about to return.
#[derive(Clone)]
pub struct ScriptNonce(pub String);

impl ScriptNonce {
    /// 128 bits of randomness as hex — the minimum the CSP specification asks
    /// for, and safe to drop straight into an HTML attribute without escaping.
    ///
    /// Public so an app writing its own middleware around
    /// [`apply_csp`] can mint one the same way the framework does.
    #[must_use]
    pub fn generate() -> Self {
        use rand::RngCore as _;
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        // Hex rather than base64 to avoid a dependency; 32 chars, still 128
        // bits of entropy, and attribute-safe by construction.
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(out, "{byte:02x}");
        }
        Self(out)
    }
}

/// The policy for one response.
///
/// In production `script-src` names the request's nonce instead of allowing
/// `'unsafe-inline'`, which is the difference between "an injected `<script>`
/// runs" and "an injected `<script>` is refused by the browser". Astro's
/// inline hydration scripts keep working because the renderer stamps the same
/// nonce onto each of them (see [`inject_script_nonce`]).
///
/// `style-src` keeps `'unsafe-inline'`. A nonce cannot cover `style="..."`
/// attributes — those are governed by `style-src-attr` — and Astro and Tailwind
/// both emit them, so removing it would break pages while buying far less than
/// the script-side change does.
///
/// Dev keeps `'unsafe-inline'` and adds `'unsafe-eval'` plus the theme dev
/// server's websocket: HMR needs both, and pages proxied straight from the dev
/// server never pass through the renderer that applies nonces.
fn content_security_policy(env: Env, nonce: &ScriptNonce) -> String {
    if env == Env::Dev {
        "default-src 'self'; \
         script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
         style-src 'self' 'unsafe-inline'; \
         font-src 'self'; \
         img-src 'self' data:; \
         object-src 'none'; \
         connect-src 'self' ws://localhost:4321 http://localhost:4321 ws://127.0.0.1:4321 http://127.0.0.1:4321 ws://0.0.0.0:4321 http://0.0.0.0:4321; \
         frame-ancestors 'none'; \
         base-uri 'self'; \
         form-action 'self';"
            .to_string()
    } else {
        format!(
            "default-src 'self'; \
             script-src 'self' 'nonce-{nonce}'; \
             style-src 'self' 'unsafe-inline'; \
             font-src 'self'; \
             img-src 'self' data:; \
             object-src 'none'; \
             frame-ancestors 'none'; \
             base-uri 'self'; \
             form-action 'self';",
            nonce = nonce.0
        )
    }
}

/// Attaches the request's Content-Security-Policy and, for HTML, stamps the
/// nonce onto every `<script>` tag in the body.
///
/// One interception point for both halves of the same mechanism: a policy
/// naming a nonce and a body carrying it must never disagree, and doing it here
/// covers every HTML response — rendered pages, the themed error page, and
/// pages proxied from a theme dev server — without the renderer or any handler
/// knowing about CSP.
#[doc(hidden)]
pub async fn apply_csp<B>(
    env: Env,
    nonce: ScriptNonce,
    res: ServiceResponse<B>,
) -> Result<ServiceResponse<actix_web::body::BoxBody>, actix_web::Error>
where
    B: MessageBody + 'static,
{
    let header = actix_web::http::header::CONTENT_SECURITY_POLICY;
    // An HTML body is the only kind that can contain a `<script>` tag, and
    // rewriting anything else would corrupt it.
    let is_html = res
        .headers()
        .get(actix_web::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    // Only when nothing more specific was set: the uploads mount serves a
    // `sandbox` policy and `serve_asset` sets its own, and neither may be
    // overwritten with a laxer one.
    let set_policy = !res.headers().contains_key(&header);

    if !is_html {
        let mut res = res.map_into_boxed_body();
        if set_policy && let Ok(value) = content_security_policy(env, &nonce).parse() {
            res.headers_mut().insert(header, value);
        }
        return Ok(res);
    }

    let (req, res) = res.into_parts();
    let (mut res, body) = res.into_parts();
    // Pages are rendered into a complete `String` before they reach here, so
    // collecting the body costs nothing it did not already cost.
    // `B::Error` is only `Into<Box<dyn Error>>`, not `Display`, so it cannot be
    // forwarded directly — a body that fails mid-collection becomes a plain 500.
    let bytes = actix_web::body::to_bytes(body).await.map_err(|_| {
        actix_web::error::ErrorInternalServerError("failed to read the response body")
    })?;

    let body = match std::str::from_utf8(&bytes) {
        Ok(html) => actix_web::body::BoxBody::new(inject_script_nonce(html, &nonce)),
        // Content-Type said HTML but the bytes are not UTF-8: pass them through
        // untouched rather than mangle them.
        Err(_) => actix_web::body::BoxBody::new(bytes),
    };

    if set_policy && let Ok(value) = content_security_policy(env, &nonce).parse() {
        res.headers_mut().insert(header, value);
    }
    // The body length changed, so a stale Content-Length would truncate it.
    res.headers_mut()
        .remove(actix_web::http::header::CONTENT_LENGTH);

    Ok(ServiceResponse::new(req, res.set_body(body)))
}

/// Adds `nonce="..."` to every `<script` opening tag that lacks one.
///
/// Run over the rendered HTML, so a theme's inline hydration scripts satisfy a
/// nonce-based `script-src` without the theme knowing anything about CSP.
/// Tags that already carry a nonce are left alone, and `<script` appearing in
/// text is not matched because the following byte must be `>` or whitespace.
///
/// Note the policy keeps `'self'`, so `<script src="/_astro/...">` is allowed
/// with or without the nonce; this exists for the *inline* ones.
fn inject_script_nonce(html: &str, nonce: &ScriptNonce) -> String {
    const TAG: &str = "<script";
    if !html.contains(TAG) {
        return html.to_string();
    }
    let attribute = format!(" nonce=\"{}\"", nonce.0);
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    while let Some(at) = rest.find(TAG) {
        let after_tag = at + TAG.len();
        // `<scripting>` or `<scriptfoo` is not a script tag.
        let boundary_ok = rest[after_tag..]
            .chars()
            .next()
            .is_some_and(|c| c == '>' || c.is_whitespace());
        out.push_str(&rest[..after_tag]);
        if boundary_ok {
            // Don't double-stamp a tag that already declares one.
            let tag_end = rest[after_tag..]
                .find('>')
                .map_or(rest.len(), |i| after_tag + i);
            if !rest[after_tag..tag_end].contains("nonce=") {
                out.push_str(&attribute);
            }
        }
        rest = &rest[after_tag..];
    }
    out.push_str(rest);
    out
}

/// Probes this app's own `/healthz` over loopback and reports the result as the
/// process exit status — `Ok` for healthy, an error (exit code 1) otherwise.
///
/// Reads only `PORT`, so a broken configuration still yields a failing probe
/// rather than a confusing configuration error.
async fn run_healthcheck() -> std::io::Result<()> {
    let port = env::var("PORT")
        .ok()
        .and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(8080);
    let url = format!("http://127.0.0.1:{port}/healthz");

    let response = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
        .map_err(|err| std::io::Error::other(format!("{url}: {err}")))?;

    if response.status().is_success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "{url}: returned {}",
            response.status()
        )))
    }
}

/// Liveness and readiness probes, for a container runtime or load balancer.
///
/// * `GET /healthz` — the process is up and serving. Touches nothing, so it
///   stays honest about liveness: a failing database must not get the container
///   killed and restarted, which would not fix anything.
/// * `GET /readyz` — the process can do useful work. Runs `SELECT 1`, so a pool
///   that cannot answer takes the instance out of rotation instead of serving
///   errors to users. Returns `503` when it fails.
///
/// Both answer `text/plain` and are excluded from the access log (see
/// [`observability::HEALTH_ROUTES`]) — a probe every few seconds would
/// otherwise be most of the log.
///
/// Registered with `configure`, **not** as a `web::scope("")`: a scope with an
/// empty prefix matches every path and answers 404 for anything it does not
/// itself route, which shadows the app's own routes and the static-asset
/// fallback. Two plain routes claim only the two paths they name.
#[doc(hidden)]
pub fn health_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/healthz",
        web::get().to(|| async { HttpResponse::Ok().content_type("text/plain").body("ok") }),
    );
    cfg.route(
        "/readyz",
        web::get().to(|data: web::Data<AppData>| async move {
            match sqlx::query_scalar::<_, i64>("SELECT 1")
                .fetch_one(&data.db)
                .await
            {
                Ok(_) => HttpResponse::Ok().content_type("text/plain").body("ready"),
                Err(err) => {
                    // Logged, because an instance dropping out of rotation is
                    // worth a line even though the response is terse.
                    error!("readiness check failed: {err}");
                    HttpResponse::ServiceUnavailable()
                        .content_type("text/plain")
                        .body("not ready")
                }
            }
        }),
    );
}

/// The `/uploads` static mount. Every response carries a `sandbox` CSP: the
/// site-wide policy allows `'unsafe-inline'` scripts (Astro needs it), so an
/// uploaded document that can carry script (SVG, HTML — extensions an app may
/// legitimately allow in `save_upload`) must never execute in the site's
/// origin when opened directly — that would be stored XSS. `sandbox` applies
/// when the file is the navigated document; embedded uses (`<img src=...>`)
/// are unaffected, since a subresource has no script context of its own.
#[doc(hidden)]
#[must_use]
pub fn uploads_service() -> impl actix_web::dev::HttpServiceFactory {
    web::scope("/uploads")
        .wrap(DefaultHeaders::new().add(("Content-Security-Policy", "sandbox")))
        .service(actix_files::Files::new("", "./uploads"))
}

async fn forward_to_dev_server(
    server: &str,
    req: &actix_web::HttpRequest,
) -> actix_web::Result<HttpResponse> {
    let url = format!("{server}{}", req.uri());
    debug!("Proxying request to Astro dev server: {url}");
    let response = reqwest::get(&url).await.map_err(|e| {
        error!("Failed to proxy to Astro dev server: {e}");
        actix_web::error::ErrorInternalServerError("Proxy error")
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(actix_web::error::ErrorNotFound("Not found on dev server"));
    }

    let content_type = response
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let body = response.bytes().await.map_err(|e| {
        error!("Failed to read body from Astro dev server: {e}");
        actix_web::error::ErrorInternalServerError("Body error")
    })?;

    let mut res =
        HttpResponse::build(actix_web::http::StatusCode::from_u16(status.as_u16()).unwrap());
    res.content_type(content_type);
    Ok(res.body(body))
}

/// How long a fingerprinted asset may be cached: the maximum a year, and
/// `immutable` so a browser does not even revalidate it. Safe because the
/// filename contains a hash of the contents — a changed file is a new URL.
const IMMUTABLE_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
/// Everything else the theme serves. Short, and revalidated, because the URL
/// stays the same when the file changes.
const MUTABLE_CACHE_CONTROL: &str = "public, max-age=300, must-revalidate";

/// True when the asset's URL contains a content hash, so its bytes can never
/// change under the same name.
///
/// Astro emits `_astro/Layout.CwkyWajQ.css` — name, hash, extension. Matching
/// the directory rather than trying to recognise a hash keeps this honest: a
/// wrong guess here would either pin a mutable file for a year or throw away
/// the caching on every hashed one.
fn is_fingerprinted(path: &str) -> bool {
    path.starts_with("_astro/") || path.contains("/_astro/")
}

#[doc(hidden)]
pub fn serve_asset(
    themes: &themes::ThemeStack,
    path: &str,
    method: &str,
) -> actix_web::Result<HttpResponse> {
    if method != "GET" && method != "HEAD" {
        return Ok(HttpResponse::MethodNotAllowed().finish());
    }

    let contents = themes
        .asset(path)
        .ok_or_else(|| actix_web::error::ErrorNotFound("File not found"))?;

    let content_type = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream");

    // Without this every navigation re-downloaded every asset in full — a
    // 44 KB stylesheet on each page load, with nothing telling the browser it
    // was allowed to keep the copy it already had.
    let cache_control = if is_fingerprinted(path) {
        IMMUTABLE_CACHE_CONTROL
    } else {
        MUTABLE_CACHE_CONTROL
    };

    Ok(HttpResponse::Ok()
        .content_type(content_type)
        .insert_header((actix_web::http::header::CACHE_CONTROL, cache_control))
        .insert_header((
            "Content-Security-Policy",
            "default-src 'self'; \
             script-src 'self' 'unsafe-inline'; \
             style-src 'self' 'unsafe-inline'; \
             font-src 'self'; \
             img-src 'self' data:; \
             object-src 'none'; \
             frame-ancestors 'none'; \
             base-uri 'self'; \
             form-action 'self';",
        ))
        .insert_header(("X-Content-Type-Options", "nosniff"))
        .insert_header(("X-Frame-Options", "DENY"))
        .insert_header(("Referrer-Policy", "strict-origin-when-cross-origin"))
        .body(contents.to_vec()))
}

/// Replaces an error response with the theme's error page.
///
/// Also the hand-off point for error reporting. The rendered page is a *new*
/// response, so the failure's [`ErrorDetail`](observability::ErrorDetail) — and
/// the `actix_web::Error` it came from — would be dropped here; instead the
/// detail is recovered (from whatever `AppError::error_response` attached, or
/// from the error the response still carries, or from the status itself) and
/// re-attached to the page, where `FseRootSpan::on_request_end` logs it once
/// with its full cause chain.
// The `Result` wrapper is required by `ErrorHandlers::handler`'s signature.
#[allow(clippy::unnecessary_wraps)]
fn render_error_page<B>(res: ServiceResponse<B>) -> actix_web::Result<ErrorHandlerResponse<B>>
where
    B: MessageBody + 'static,
{
    let (req, res) = res.into_parts();
    let data = req.app_data::<web::Data<AppData>>().cloned().unwrap();
    let status = res.status();

    let is_logged_in = crate::auth::read_jwt::<crate::structs::DefaultRole>(&req).is_ok();

    let (template, final_status) = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            if is_logged_in {
                ("error", StatusCode::NOT_FOUND)
            } else {
                ("public/error", StatusCode::NOT_FOUND)
            }
        }
        _ => {
            if is_logged_in {
                ("error", status)
            } else {
                ("public/error", status)
            }
        }
    };

    // Previously this read `req.extensions().get::<String>()`, which was
    // always `None`: `ResponseError::error_response` can only reach the
    // *response*'s extensions. The detailed message never actually appeared on
    // a dev error page.
    let detail = res
        .extensions()
        .get::<observability::ErrorDetail>()
        .cloned()
        .or_else(|| {
            res.error()
                .map(|err| observability::ErrorDetail::from_display(err, status))
        });

    // Dev gets the real message to debug with; prod gets the status's
    // canonical reason and nothing else, because the detailed message can
    // name tables, hosts and file paths.
    let display_error = if data.env == Env::Dev {
        detail.as_ref().map_or_else(
            || {
                final_status
                    .canonical_reason()
                    .unwrap_or("Unknown Error")
                    .to_string()
            },
            |d| d.log_message.clone(),
        )
    } else {
        final_status
            .canonical_reason()
            .unwrap_or("An unexpected error occurred")
            .to_string()
    };

    // The id the `x-request-id` header carries, so someone looking at the page
    // can quote the one string that finds this request in the logs.
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(std::string::ToString::to_string);

    Ok(ErrorHandlerResponse::Future(Box::pin(async move {
        let mut ctx = serde_json::json!({
            "status": final_status.as_u16(),
            "error": display_error,
            "request_id": request_id,
        });

        data.inject_request_context(&req, &mut ctx);
        if let Some(injector) = &data.context_injector {
            injector(&req, &mut ctx);
        }

        let res_template = data.render_template(template, &ctx).await;
        let mut res = res_template;
        *res.status_mut() = final_status;
        // Carry the failure forward so the request span still reports it,
        // even though this response is not the one that failed.
        if let Some(detail) = detail {
            res.extensions_mut().insert(detail);
        }

        let res = ServiceResponse::new(req, res).map_into_right_body();

        Ok(res)
    })))
}

/// Loads the `.env` file, returning where it was found.
///
/// Returns the path rather than logging it, because this runs *before* the
/// subscriber exists — `RUST_LOG` and the other logging settings live in that
/// very file, so it has to be read first, and a log line emitted here would go
/// nowhere. The caller reports it once logging is up.
fn load_env_file() -> Option<std::path::PathBuf> {
    dotenv().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    static BASE_THEME: Dir<'_> =
        include_dir::include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/themes/base");
    static CHILD_THEME: Dir<'_> =
        include_dir::include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/themes/child");

    fn test_stack() -> themes::ThemeStack {
        testing::theme_stack([
            themes::Theme::embedded(&BASE_THEME),
            themes::Theme::embedded(&CHILD_THEME),
        ])
        .unwrap()
    }

    #[test]
    fn theme_templates_register_by_path_and_broken_ones_are_skipped() {
        let tera = test_stack().tera();
        let names: Vec<&str> = tera.get_template_names().collect();
        // `index.html` -> "index", `login/index.html` -> "login".
        assert!(names.contains(&"index"));
        assert!(names.contains(&"login"));
        assert!(names.contains(&"@base/login"));
        // The syntactically broken template is skipped instead of panicking.
        assert!(!names.contains(&"broken"));
    }

    #[test]
    fn load_themes_reports_broken_templates_instead_of_skipping_them() {
        let err = testing::load_themes([
            themes::Theme::embedded(&BASE_THEME),
            themes::Theme::embedded(&CHILD_THEME),
        ])
        .unwrap_err();
        assert!(
            err.contains("broken"),
            "error should name the broken template: {err}"
        );
    }

    #[test]
    fn load_themes_succeeds_when_every_template_parses() {
        let ok = themes::Theme::new(themes::ThemeManifest {
            name: "ok".into(),
            parent: None,
            version: None,
            description: None,
        })
        .with_file("index.html", "hi {{ value }}");
        let tera = testing::load_themes([ok]).unwrap();
        assert!(tera.get_template_names().any(|n| n == "index"));
    }

    #[test]
    fn child_theme_overrides_and_includes_the_parent_template() {
        let tera = test_stack().tera();
        let context =
            Context::from_serialize(serde_json::json!({ "value": "<script>alert(1)</script>" }))
                .unwrap();

        let html = tera.render("login", &context).unwrap();
        assert!(html.contains("child login"));
        assert!(
            html.contains("<html><body>login"),
            "parent template included: {html}"
        );
        // Autoescaping applies to every theme's templates.
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
        // Untouched templates come from the parent.
        assert!(tera.render("index", &context).is_ok());
    }

    #[test]
    fn serve_asset_serves_theme_files_with_hardened_headers() {
        let stack = test_stack();
        let res = serve_asset(&stack, "_astro/app.css", "GET").unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let headers = res.headers();
        assert_eq!(headers.get("Content-Type").unwrap(), "text/css");
        assert_eq!(headers.get("X-Content-Type-Options").unwrap(), "nosniff");
        assert!(headers.get("Content-Security-Policy").is_some());
        // Child assets are served too.
        assert!(serve_asset(&stack, "custom.css", "GET").is_ok());
    }

    #[test]
    fn serve_asset_rejects_non_read_methods() {
        let res = serve_asset(&test_stack(), "_astro/app.css", "POST").unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn serve_asset_never_exposes_templates_or_the_manifest() {
        let stack = test_stack();
        assert!(serve_asset(&stack, "no-such-file.css", "GET").is_err());
        assert!(serve_asset(&stack, "index.html", "GET").is_err());
        assert!(serve_asset(&stack, "theme.json", "GET").is_err());
    }

    #[test]
    fn script_nonce_is_stamped_on_inline_scripts_only_once() {
        let nonce = ScriptNonce("abc123".to_string());
        // Bare inline script.
        assert_eq!(
            inject_script_nonce("<script>hydrate()</script>", &nonce),
            r#"<script nonce="abc123">hydrate()</script>"#
        );
        // A tag with attributes keeps them.
        assert_eq!(
            inject_script_nonce(r#"<script type="module" src="/a.js"></script>"#, &nonce),
            r#"<script nonce="abc123" type="module" src="/a.js"></script>"#
        );
        // Several tags all get it.
        let out = inject_script_nonce("<script>a</script><script>b</script>", &nonce);
        assert_eq!(out.matches(r#"nonce="abc123""#).count(), 2);
    }

    #[test]
    fn script_nonce_leaves_non_script_tags_and_existing_nonces_alone() {
        let nonce = ScriptNonce("abc123".to_string());
        // A tag that merely starts with the same letters is not a script tag.
        let html = "<scripting>x</scripting>";
        assert_eq!(inject_script_nonce(html, &nonce), html);
        // An already-nonced tag is not stamped twice (which would be invalid
        // HTML and could shadow the real value).
        let html = r#"<script nonce="other">x</script>"#;
        assert_eq!(inject_script_nonce(html, &nonce), html);
        // Nothing to do at all.
        assert_eq!(inject_script_nonce("<p>hi</p>", &nonce), "<p>hi</p>");
        // The word in text is not a tag.
        assert_eq!(
            inject_script_nonce("use a &lt;script&gt; tag", &nonce),
            "use a &lt;script&gt; tag"
        );
    }

    #[test]
    fn production_csp_names_the_nonce_and_drops_unsafe_inline_scripts() {
        let nonce = ScriptNonce("deadbeef".to_string());
        let prod = content_security_policy(Env::Prod, &nonce);
        assert!(
            prod.contains("script-src 'self' 'nonce-deadbeef'"),
            "{prod}"
        );
        // The whole point: an injected inline script is refused.
        assert!(
            !prod.contains("script-src 'self' 'unsafe-inline'"),
            "prod script-src must not allow inline: {prod}"
        );
        // Styles keep it: a nonce cannot cover `style="..."` attributes.
        assert!(prod.contains("style-src 'self' 'unsafe-inline'"), "{prod}");

        // Dev keeps inline + eval for HMR, and never sees a nonce.
        let dev = content_security_policy(Env::Dev, &nonce);
        assert!(dev.contains("'unsafe-eval'"), "{dev}");
        assert!(!dev.contains("nonce-"), "{dev}");
    }

    #[test]
    fn each_request_gets_a_fresh_high_entropy_nonce() {
        let a = ScriptNonce::generate();
        let b = ScriptNonce::generate();
        assert_ne!(a.0, b.0, "a reused nonce is no better than 'unsafe-inline'");
        // 128 bits as hex.
        assert_eq!(a.0.len(), 32);
        assert!(a.0.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn only_fingerprinted_assets_are_cached_forever() {
        // Astro's hashed output: the name changes when the bytes do.
        assert!(is_fingerprinted("_astro/Layout.CwkyWajQ.css"));
        assert!(is_fingerprinted("_astro/client.BhK2.js"));
        // Everything else keeps its URL across edits, so it must revalidate.
        assert!(!is_fingerprinted("favicon.ico"));
        assert!(!is_fingerprinted("images/logo.svg"));
        assert!(!is_fingerprinted("robots.txt"));
    }

    #[test]
    fn inject_page_props_fills_the_placeholder_once() {
        let html = format!("<head>{PAGE_PROPS_TAG}</head>");
        let ctx = serde_json::json!({ "rows": [1, 2], "error": null });
        let out = inject_page_props(html, &ctx);
        assert!(out.contains(r#"<script type="application/json" id="__fse-props__">{"error":null,"rows":[1,2]}</script>"#));
    }

    #[test]
    fn inject_page_props_escapes_script_breakout() {
        let html = format!("<head>{PAGE_PROPS_TAG}</head>");
        let ctx = serde_json::json!({ "evil": "</script><script>alert(1)</script>" });
        let out = inject_page_props(html, &ctx);
        assert!(!out.contains("</script><script>alert(1)"));
        // The `<` of every context value is emitted as the JSON escape <.
        assert!(out.contains("\\u003c/script>\\u003cscript>alert(1)"));
    }

    #[test]
    fn inject_page_props_leaves_pages_without_placeholder_untouched() {
        let html = "<head><title>x</title></head>".to_string();
        assert_eq!(
            inject_page_props(html.clone(), &serde_json::json!({})),
            html
        );
    }

    /// Pins every Tera form the frontend's fse-ssr compiler emits
    /// (see `fse-ssr/src/runtime.ts`). If this test breaks
    /// after a Tera upgrade, the emitter must be adapted too.
    #[test]
    fn fse_ssr_emitted_tera_grammar_renders() {
        let mut tera = Tera::default();
        tera.autoescape_on(vec![""]);
        tera.add_raw_template(
            "page",
            concat!(
                // scalar + attribute position
                "<a href=\"/users/{{ id }}\">{{ email }}</a>",
                // `??` fallback on values
                "[{{ missing | default(value='Home') }}]",
                // `{cond && <...>}` on a possibly-absent key
                "{% if error | default(value=false) %}ERR:{{ error }}{% endif %}",
                // `{!cond && <...>}` — no parens: Tera only groups math
                "{% if not missing | default(value=false) %}ANON{% endif %}",
                // combined conditions stay flat; `and` binds tighter than `or`
                "{% if flag | default(value=false) and role == 'admin' %}BOTH{% endif %}",
                // loops with comparisons, computed keys and defaults
                "{% for it0 in roles %}<option{% if it0.value == role %} selected{% endif %}>",
                "{{ t.roles[it0.value] | default(value=it0.value) }}</option>{% endfor %}",
                // `.length`
                "({{ roles | length }})",
            ),
        )
        .unwrap();

        let context = Context::from_serialize(serde_json::json!({
            "id": 7,
            "email": "a@b.c",
            "error": "boom",
            "flag": true,
            "role": "admin",
            "roles": [
                { "value": "admin" },
                { "value": "user" },
            ],
            "t": { "roles": { "admin": "Admin" } },
        }))
        .unwrap();

        let html = tera.render("page", &context).unwrap();
        assert!(html.contains("<a href=\"/users/7\">a@b.c</a>"));
        assert!(html.contains("[Home]"));
        assert!(html.contains("ERR:boom"));
        assert!(html.contains("ANON"));
        assert!(html.contains("BOTH"));
        assert!(html.contains("<option selected>Admin</option>"));
        assert!(html.contains("<option>user</option>"));
        assert!(html.contains("(2)"));
    }
}
