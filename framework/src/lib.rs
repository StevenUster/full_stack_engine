#![deny(warnings, unused_imports, dead_code, clippy::all, clippy::pedantic)]

use actix_governor::Governor;
use actix_web::{
    App, HttpMessage, HttpResponse, HttpServer,
    body::MessageBody,
    dev::ServiceResponse,
    http::StatusCode,
    middleware::{DefaultHeaders, ErrorHandlerResponse, ErrorHandlers, NormalizePath},
    web,
};
use dotenv::dotenv;
use include_dir::Dir;
use log::{debug, error, info};
use sqlx::sqlite::SqlitePool;
use std::{env, fs};
use tera::{Context, Tera};
use tokio_cron_scheduler::JobScheduler;

pub mod auth;
pub mod cron;
pub mod error;
pub mod i18n;
pub mod mail;
pub mod prelude;
pub mod rate_limiter;
pub mod roles;
pub mod structs;
pub mod uploads;

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
    pub env: Env,
    pub domain: String,
    pub protocol: String,
    pub jwt_secret: String,
    pub smtp_from: String,
    pub email_verification_enabled: bool,
    pub context_injector: Option<std::sync::Arc<ContextInjectorFn>>,
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
        if self.env == Env::Dev {
            let path = if template_name == "index" {
                String::new()
            } else {
                template_name.replace('_', "/")
            };
            let url = format!("http://localhost:4321/{path}");

            let astro_html = match reqwest::get(&url).await {
                Ok(response) => {
                    if response.status().is_success() {
                        match response.text().await {
                            Ok(html) => html,
                            Err(err) => {
                                error!("Failed to read response from Astro dev server: {err}");
                                return HttpResponse::InternalServerError()
                                    .body("Failed to read response");
                            }
                        }
                    } else {
                        error!("Astro dev server returned status: {}", response.status());
                        return HttpResponse::InternalServerError().body("Astro dev server error");
                    }
                }
                Err(err) => {
                    error!("Failed to connect to Astro dev server at {url}: {err}");
                    return HttpResponse::InternalServerError()
                        .body("Failed to connect to Astro dev server");
                }
            };

            let mut tera_temp = Tera::default();
            // Template names never carry a `.html` suffix (see `add_templates`), so Tera's
            // default suffix-based autoescape detection never matches. An empty suffix is a
            // suffix of every string, so this forces escaping for every template regardless
            // of its name. Templates that intentionally emit raw HTML use the `safe` filter.
            tera_temp.autoescape_on(vec![""]);
            if let Err(err) = tera_temp.add_raw_template(template_name, &astro_html) {
                error!("Failed to add Astro HTML as Tera template: {err}");
                return HttpResponse::InternalServerError().body("Failed to add template");
            }

            let context = match Context::from_serialize(&value) {
                Ok(ctx) => ctx,
                Err(err) => {
                    error!("Context serialization error: {err}");
                    return HttpResponse::InternalServerError().body("Context serialization error");
                }
            };

            match tera_temp.render(template_name, &context) {
                Ok(html) => HttpResponse::Ok()
                    .content_type("text/html")
                    .body(inject_page_props(html, &value)),
                Err(err) => {
                    error!("Template rendering error: {err}");
                    HttpResponse::InternalServerError().body("Template rendering error")
                }
            }
        } else {
            let context = match Context::from_serialize(&value) {
                Ok(ctx) => ctx,
                Err(err) => {
                    error!("Context serialization error: {err}");
                    return HttpResponse::InternalServerError().finish();
                }
            };

            let template_name = template_name.replace('_', "/");
            match self.tera.render(&template_name, &context) {
                Ok(html) => HttpResponse::Ok()
                    .content_type("text/html")
                    .body(inject_page_props(html, &value)),
                Err(err) => {
                    error!("Template rendering error ({template_name}): {err}");
                    HttpResponse::InternalServerError().finish()
                }
            }
        }
    }

    /// Renders an email template to an HTML string (via the Astro dev server
    /// in dev, from the embedded templates in prod).
    ///
    /// # Errors
    ///
    /// Returns a description of what failed (dev-server connection, context
    /// serialization, or template rendering).
    pub async fn render_email<T: serde::Serialize>(
        &self,
        template_name: &str,
        context_data: &T,
    ) -> Result<String, String> {
        if self.env == Env::Dev {
            let path = template_name.replace('_', "/");
            let url = format!("http://localhost:4321/{path}");

            let astro_html = reqwest::get(&url)
                .await
                .map_err(|e| format!("Failed to connect to Astro dev server: {e}"))?
                .text()
                .await
                .map_err(|e| format!("Failed to read Astro dev server response: {e}"))?;

            let mut tera_temp = Tera::default();
            tera_temp.autoescape_on(vec![""]);
            tera_temp
                .add_raw_template(template_name, &astro_html)
                .map_err(|e| format!("Failed to add email template: {e}"))?;

            let context = Context::from_serialize(context_data)
                .map_err(|e| format!("Context serialization error: {e}"))?;

            tera_temp
                .render(template_name, &context)
                .map_err(|e| format!("Email template rendering error: {e}"))
        } else {
            let context = Context::from_serialize(context_data)
                .map_err(|e| format!("Context serialization error: {e}"))?;

            let template_name = template_name.replace('_', "/");
            self.tera
                .render(&template_name, &context)
                .map_err(|e| format!("Email template rendering error ({template_name}): {e}"))
        }
    }
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

pub struct FrameworkApp {
    dist_dir: &'static Dir<'static>,
    configure_fn: Option<ConfigureFn>,
    cronjobs_fn: Option<CronjobsFn>,
    context_injector: Option<std::sync::Arc<ContextInjectorFn>>,
    migrator: Option<sqlx::migrate::Migrator>,
    rate_limit_exempt_prefixes: Vec<String>,
}

impl FrameworkApp {
    #[must_use]
    pub fn new(dist_dir: &'static Dir<'static>) -> Self {
        Self {
            dist_dir,
            configure_fn: None,
            cronjobs_fn: None,
            context_injector: None,
            migrator: None,
            rate_limit_exempt_prefixes: Vec::new(),
        }
    }

    /// Path prefixes exempt from the site-wide rate limiter (see
    /// [`rate_limiter::global_rate_limiter`]). Use for routes that are hit
    /// legitimately from a single IP at high volume — e.g. a public `/api`
    /// consumed server-side by an SSR site, which would otherwise be throttled
    /// as one client. An exempt prefix has **no** per-IP limit, so keep the list
    /// tight.
    ///
    /// ```ignore
    /// FrameworkApp::new(&DIST_DIR).rate_limit_exempt_prefixes(["/api"]).run().await
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
    /// FrameworkApp::new(&DIST_DIR).migrator(sqlx::migrate!()).run().await
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
    /// migrations, or the cron scheduler fail to initialize — failing loudly
    /// at boot instead of running half-configured.
    pub async fn run(mut self) -> std::io::Result<()> {
        env_logger::init_from_env(env_logger::Env::new().default_filter_or("debug"));
        load_env_file();

        info!("Starting application...");

        let domain = env::var("DOMAIN").expect("DOMAIN not set in .env file");
        let protocol = env::var("PROTOCOL").expect("PROTOCOL not set in .env file");
        let jwt_secret = env::var("JWT_SECRET").expect("JWT_SECRET not set in .env file");
        let database_url = env::var("DATABASE_URL").expect("DATABASE_URL not set in .env file");

        let db_pool = init_db(&database_url, self.migrator.take()).await?;

        let mut tera = Tera::default();
        // Template names never carry a `.html` suffix (see `add_templates`), so Tera's
        // default suffix-based autoescape detection never matches. An empty suffix is a
        // suffix of every string, so this forces escaping for every template regardless
        // of its name. Templates that intentionally emit raw HTML use the `safe` filter.
        tera.autoescape_on(vec![""]);
        add_templates(&mut tera, self.dist_dir);

        let env = parse_env(env::var("ENV").ok().as_deref());

        start_cron_scheduler(self.cronjobs_fn.take(), &database_url).await;

        let dist_dir = self.dist_dir;
        let configure_fn = self.configure_fn.map(std::sync::Arc::new);
        let context_injector = self.context_injector.clone();

        // Built once and shared across all workers (the config holds an Arc to
        // the token buckets), so the site-wide per-IP limit is enforced for the
        // whole process rather than per worker thread. Configured exempt
        // prefixes (e.g. a public `/api`) skip the limiter entirely.
        let global_rate_config =
            rate_limiter::global_rate_limiter(&self.rate_limit_exempt_prefixes);

        HttpServer::new(move || {
            let mut app = App::new()
                .app_data(web::Data::new(AppData {
                    tera: tera.clone(),
                    db: db_pool.clone(),
                    env,
                    domain: domain.clone(),
                    protocol: protocol.clone(),
                    jwt_secret: jwt_secret.clone(),
                    smtp_from: env::var("SMTP_USER").unwrap_or_default(),
                    email_verification_enabled: env::var("EMAIL_VERIFICATION_ENABLED")
                        .unwrap_or_else(|_| "false".to_string())
                        == "true",
                    context_injector: context_injector.clone(),
                }))
                .wrap(NormalizePath::trim())
                .wrap(
                    ErrorHandlers::new()
                        .handler(StatusCode::INTERNAL_SERVER_ERROR, render_error_page)
                        .handler(StatusCode::NOT_FOUND, render_error_page)
                        .handler(StatusCode::BAD_REQUEST, render_error_page)
                        .handler(StatusCode::UNAUTHORIZED, render_error_page)
                        .handler(StatusCode::FORBIDDEN, render_error_page),
                )
                .wrap(security_headers(env))
                // Outermost layer: reject per-IP floods before any routing or
                // request processing happens. Shared buckets across workers.
                .wrap(Governor::new(&global_rate_config));

            if let Some(ref configure_fn) = configure_fn {
                let cf = configure_fn.clone();
                app = app.configure(move |cfg| (cf)(cfg));
            }

            // Public uploaded files (see `uploads::save_upload`, which returns
            // `/uploads/...` paths). Registered after the app's own routes so
            // an app route wins on conflict. `actix-files` rejects path
            // traversal, and only this directory is exposed — private files
            // (`data/`) stay unreachable.
            app.service(actix_files::Files::new("/uploads", "./uploads"))
                .service(web::scope("/_astro").route(
                    "/{path:.*}",
                    web::get().to(move |req: actix_web::HttpRequest| async move {
                        if env == Env::Dev
                            && let Ok(res) = forward_to_dev_server(&req).await
                        {
                            return Ok(res);
                        }
                        let path = req.path().trim_start_matches('/');
                        serve_from_dist(dist_dir, path, req.method().as_str())
                    }),
                ))
                .default_service(web::to(move |req: actix_web::HttpRequest| async move {
                    if env == Env::Dev
                        && let Ok(res) = forward_to_dev_server(&req).await
                    {
                        return Ok(res);
                    }

                    let path = req.path().trim_start_matches('/');
                    match serve_from_dist(dist_dir, path, req.method().as_str()) {
                        Ok(res) => Ok(res),
                        Err(_) => {
                            Ok::<HttpResponse, actix_web::Error>(HttpResponse::NotFound().finish())
                        }
                    }
                }))
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
    database_url: &str,
    embedded_migrator: Option<sqlx::migrate::Migrator>,
) -> std::io::Result<SqlitePool> {
    let db_file = database_url.trim_start_matches("sqlite:");

    if let Some(dir) = std::path::Path::new(db_file).parent() {
        fs::create_dir_all(dir)?;
    }

    if !std::path::Path::new(db_file).exists() {
        fs::File::create(db_file)?;
    }

    let db_pool = SqlitePool::connect(database_url)
        .await
        .expect("Failed to create database pool");

    // Prefer migrations embedded in the binary (via `.migrator(...)`); fall
    // back to reading them from disk at runtime when none were supplied.
    let migrator = if let Some(migrator) = embedded_migrator {
        migrator
    } else {
        let migrations_path =
            env::var("MIGRATIONS_DIR").unwrap_or_else(|_| "./migrations".to_string());
        sqlx::migrate::Migrator::new(std::path::Path::new(&migrations_path))
            .await
            .expect("Failed to load migrations")
    };
    migrator
        .run(&db_pool)
        .await
        .expect("Failed to run database migrations");

    sqlx::query("PRAGMA foreign_keys = 1;")
        .execute(&db_pool)
        .await
        .expect("Failed to run PRAGMA foreign_keys = 1;");

    sqlx::query("PRAGMA journal_mode=WAL;")
        .execute(&db_pool)
        .await
        .expect("Failed to set WAL mode");

    Ok(db_pool)
}

/// Registers the app's cron jobs (on their own DB pool) and starts the
/// scheduler if any job was added. Panics on failure — see [`FrameworkApp::run`].
async fn start_cron_scheduler(cronjobs_fn: Option<CronjobsFn>, database_url: &str) {
    let mut sched = JobScheduler::new()
        .await
        .expect("Failed to create job scheduler");

    if let Some(cronjobs_fn) = cronjobs_fn {
        let cron_db_pool = SqlitePool::connect(database_url)
            .await
            .expect("Failed to create cron database pool");

        (cronjobs_fn)(sched.clone(), cron_db_pool)
            .await
            .expect("Failed to add cron jobs");
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

/// Only an explicit `ENV=dev` opts into dev mode. Anything else — unset,
/// "prod", or a typo like "production" — gets the hardened production
/// behaviour (secure cookies, no dev-server proxy, no detailed error
/// messages), so a misconfiguration fails safe.
fn parse_env(value: Option<&str>) -> Env {
    match value {
        Some("dev") => Env::Dev,
        _ => Env::Prod,
    }
}

/// Hardened response headers applied to every response.
fn security_headers(env: Env) -> DefaultHeaders {
    let headers = DefaultHeaders::new()
        .add(("X-Content-Type-Options", "nosniff"))
        .add(("X-Frame-Options", "DENY"))
        .add(("Referrer-Policy", "strict-origin-when-cross-origin"));

    if env == Env::Dev {
        headers.add((
            "Content-Security-Policy",
            "default-src 'self'; \
             script-src 'self' 'unsafe-inline' 'unsafe-eval'; \
             style-src 'self' 'unsafe-inline'; \
             font-src 'self'; \
             img-src 'self' data:; \
             object-src 'none'; \
             connect-src 'self' ws://localhost:4321 http://localhost:4321 ws://127.0.0.1:4321 http://127.0.0.1:4321 ws://0.0.0.0:4321 http://0.0.0.0:4321; \
             frame-ancestors 'none'; \
             base-uri 'self'; \
             form-action 'self';",
        ))
    } else {
        // `script-src`/`style-src` keep `'unsafe-inline'` because Astro
        // emits inline hydration scripts and inline styles; removing it
        // would require a nonce threaded through the render pipeline.
        // Everything else is locked down: no plugins (`object-src`), no
        // framing, and self-only base/form targets.
        headers.add((
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
    }
}

// A bad template is logged and skipped rather than crashing the app at boot:
// one broken page must not take down every other route.
fn add_templates(tera: &mut Tera, dir: &Dir) {
    for file in dir.files() {
        if let Some(ext) = file.path().extension()
            && ext == "html"
        {
            let Some(path) = file.path().to_str() else {
                error!(
                    "Skipping template with non-UTF-8 path: {}",
                    file.path().display()
                );
                continue;
            };
            let path = path.replace('\\', "/");
            let name = if path == "index.html" {
                "index".to_string()
            } else if let Some(stripped) = path.strip_suffix("/index.html") {
                stripped.to_string()
            } else if let Some(stripped) = path.strip_suffix(".html") {
                stripped.to_string()
            } else {
                path
            };

            debug!("Registering template: {name}");
            let Some(content) = file.contents_utf8() else {
                error!("Skipping template with non-UTF-8 contents: {name}");
                continue;
            };
            if let Err(err) = tera.add_raw_template(&name, content) {
                error!("Skipping invalid template {name}: {err}");
            }
        }
    }
    for subd in dir.dirs() {
        add_templates(tera, subd);
    }
}

async fn forward_to_dev_server(req: &actix_web::HttpRequest) -> actix_web::Result<HttpResponse> {
    let url = format!("http://localhost:4321{}", req.uri());
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

fn serve_from_dist(
    dist_dir: &Dir<'_>,
    path: &str,
    method: &str,
) -> actix_web::Result<HttpResponse> {
    if method != "GET" && method != "HEAD" {
        return Ok(HttpResponse::MethodNotAllowed().finish());
    }

    let file = dist_dir
        .get_file(path)
        .ok_or_else(|| actix_web::error::ErrorNotFound("File not found"))?;

    let content_type = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream");

    Ok(HttpResponse::Ok()
        .content_type(content_type)
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
        .body(file.contents().to_vec()))
}

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
                ("public_error", StatusCode::NOT_FOUND)
            }
        }
        _ => {
            if is_logged_in {
                ("error", status)
            } else {
                ("public_error", status)
            }
        }
    };

    let error_msg = req.extensions().get::<String>().cloned();
    if let Some(ref msg) = error_msg {
        error!("Error [{status}]: {msg}");
    }

    let display_error = if data.env == Env::Dev {
        error_msg.unwrap_or_else(|| {
            final_status
                .canonical_reason()
                .unwrap_or("Unknown Error")
                .to_string()
        })
    } else {
        final_status
            .canonical_reason()
            .unwrap_or("An unexpected error occurred")
            .to_string()
    };

    Ok(ErrorHandlerResponse::Future(Box::pin(async move {
        let mut ctx = serde_json::json!({
            "status": final_status.as_u16(),
            "error": display_error,
        });

        if let Some(injector) = &data.context_injector {
            injector(&req, &mut ctx);
        }

        let res_template = data.render_template(template, &ctx).await;
        let mut res = res_template;
        *res.status_mut() = final_status;

        let res = ServiceResponse::new(req, res).map_into_right_body();

        Ok(res)
    })))
}

fn load_env_file() {
    match dotenv() {
        Ok(path) => debug!(".env file loaded from: {}", path.display()),
        Err(_) => debug!("No .env file found, relying on system environment variables."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_DIST: Dir<'_> =
        include_dir::include_dir!("$CARGO_MANIFEST_DIR/tests/fixtures/templates");

    #[test]
    fn parse_env_only_explicit_dev_opts_into_dev_mode() {
        assert!(parse_env(Some("dev")) == Env::Dev);
        // Everything else fails safe to prod: unset, prod, typos, wrong case.
        assert!(parse_env(None) == Env::Prod);
        assert!(parse_env(Some("prod")) == Env::Prod);
        assert!(parse_env(Some("production")) == Env::Prod);
        assert!(parse_env(Some("development")) == Env::Prod);
        assert!(parse_env(Some("DEV")) == Env::Prod);
        assert!(parse_env(Some("")) == Env::Prod);
    }

    fn test_tera() -> Tera {
        let mut tera = Tera::default();
        tera.autoescape_on(vec![""]);
        add_templates(&mut tera, &TEST_DIST);
        tera
    }

    #[test]
    fn add_templates_registers_html_files_and_skips_broken_ones() {
        let tera = test_tera();
        let names: Vec<&str> = tera.get_template_names().collect();
        // `index.html` -> "index", `login/index.html` -> "login".
        assert!(names.contains(&"index"));
        assert!(names.contains(&"login"));
        // The syntactically broken template is skipped instead of panicking.
        assert!(!names.contains(&"broken"));
    }

    #[test]
    fn templates_escape_variables_by_default() {
        let tera = test_tera();
        let context = Context::from_serialize(serde_json::json!({
            "value": "<script>alert(1)</script>",
        }))
        .unwrap();

        let html = tera.render("login", &context).unwrap();
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn serve_from_dist_serves_embedded_files_with_hardened_headers() {
        let res = serve_from_dist(&TEST_DIST, "index.html", "GET").unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let headers = res.headers();
        assert_eq!(headers.get("Content-Type").unwrap(), "text/html");
        assert_eq!(headers.get("X-Content-Type-Options").unwrap(), "nosniff");
        assert!(headers.get("Content-Security-Policy").is_some());
    }

    #[test]
    fn serve_from_dist_rejects_non_read_methods() {
        let res = serve_from_dist(&TEST_DIST, "index.html", "POST").unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn serve_from_dist_missing_file_is_an_error() {
        assert!(serve_from_dist(&TEST_DIST, "no-such-file.html", "GET").is_err());
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
    /// (see `starter/src/frontend/fse-ssr/runtime.ts`). If this test breaks
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
