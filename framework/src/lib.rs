#![deny(warnings, unused_imports, dead_code, clippy::all, clippy::pedantic)]

use actix_web::{
    App, HttpMessage, HttpResponse, HttpServer,
    body::MessageBody,
    dev::ServiceResponse,
    http::StatusCode,
    middleware::{ErrorHandlerResponse, ErrorHandlers},
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
pub mod prelude;
pub mod rate_limiter;
pub mod structs;

#[derive(Clone, PartialEq, serde::Serialize)]
pub enum Env {
    Dev,
    Prod,
}

pub struct AppData {
    pub tera: Tera,
    pub db: SqlitePool,
    pub env: Env,
    pub domain: String,
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
        if self.env == Env::Dev {
            let path = if template_name == "index" {
                String::new()
            } else {
                template_name.replace("_", "/")
            };
            let url = format!("http://localhost:4321/{}", path);

            let astro_html = match reqwest::get(&url).await {
                Ok(response) => {
                    if response.status().is_success() {
                        match response.text().await {
                            Ok(html) => html,
                            Err(err) => {
                                error!("Failed to read response from Astro dev server: {}", err);
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
                    error!("Failed to connect to Astro dev server at {}: {}", url, err);
                    return HttpResponse::InternalServerError()
                        .body("Failed to connect to Astro dev server");
                }
            };

            let mut tera_temp = Tera::default();
            if let Err(err) = tera_temp.add_raw_template(template_name, &astro_html) {
                error!("Failed to add Astro HTML as Tera template: {}", err);
                return HttpResponse::InternalServerError().body("Failed to add template");
            }

            let context = match Context::from_serialize(context_data) {
                Ok(ctx) => ctx,
                Err(err) => {
                    error!("Context serialization error: {}", err);
                    return HttpResponse::InternalServerError().body("Context serialization error");
                }
            };

            match tera_temp.render(template_name, &context) {
                Ok(html) => HttpResponse::Ok().content_type("text/html").body(html),
                Err(err) => {
                    error!("Template rendering error: {}", err);
                    HttpResponse::InternalServerError().body("Template rendering error")
                }
            }
        } else {
            let context = match Context::from_serialize(context_data) {
                Ok(ctx) => ctx,
                Err(err) => {
                    error!("Context serialization error: {}", err);
                    return HttpResponse::InternalServerError().finish();
                }
            };

            let template_name = template_name.replace("_", "/");
            match self.tera.render(&template_name, &context) {
                Ok(html) => HttpResponse::Ok().content_type("text/html").body(html),
                Err(err) => {
                    error!("Template rendering error ({}): {}", template_name, err);
                    HttpResponse::InternalServerError().finish()
                }
            }
        }
    }
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
}

impl FrameworkApp {
    pub fn new(dist_dir: &'static Dir<'static>) -> Self {
        Self {
            dist_dir,
            configure_fn: None,
            cronjobs_fn: None,
        }
    }

    /// Register a route configuration function (like `services::configure`)
    pub fn configure<F>(mut self, f: F) -> Self
    where
        F: Fn(&mut web::ServiceConfig) + Send + Sync + 'static,
    {
        self.configure_fn = Some(Box::new(f));
        self
    }

    /// Register an async cronjobs setup function
    pub fn cronjobs<F, Fut>(mut self, f: F) -> Self
    where
        F: FnOnce(JobScheduler, SqlitePool) -> Fut + 'static,
        Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>> + 'static,
    {
        self.cronjobs_fn = Some(Box::new(move |sched, pool| Box::pin(f(sched, pool))));
        self
    }

    /// Start the framework: loads env, database, cron, and HTTP server
    pub async fn run(self) -> std::io::Result<()> {
        load_env_file();
        env_logger::init_from_env(env_logger::Env::new().default_filter_or("debug"));

        info!("Starting application...");

        let domain = env::var("DOMAIN").expect("DOMAIN not set in .env file");
        let database_url = env::var("DATABASE_URL").expect("DATABASE_URL not set in .env file");
        let db_file = database_url.trim_start_matches("sqlite:");

        if let Some(dir) = std::path::Path::new(db_file).parent() {
            fs::create_dir_all(dir)?;
        }

        if !std::path::Path::new(db_file).exists() {
            fs::File::create(db_file)?;
        }

        let db_pool = SqlitePool::connect(&database_url)
            .await
            .expect("Failed to create database pool");

        let migrations_path =
            env::var("MIGRATIONS_DIR").unwrap_or_else(|_| "./migrations".to_string());
        sqlx::migrate::Migrator::new(std::path::Path::new(&migrations_path))
            .await
            .expect("Failed to load migrations")
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

        let mut tera = Tera::default();
        add_templates(&mut tera, self.dist_dir);

        let env = match env::var("ENV") {
            Ok(val) => match val.as_str() {
                "prod" => Env::Prod,
                _ => Env::Dev,
            },
            Err(_) => Env::Prod,
        };

        // Cron scheduler
        let mut sched = JobScheduler::new()
            .await
            .expect("Failed to create job scheduler");

        if let Some(cronjobs_fn) = self.cronjobs_fn {
            let cron_db_pool = SqlitePool::connect(&database_url)
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

        let dist_dir = self.dist_dir;
        let configure_fn = self.configure_fn.map(std::sync::Arc::new);

        HttpServer::new(move || {
            let mut app = App::new()
                .app_data(web::Data::new(AppData {
                    tera: tera.clone(),
                    db: db_pool.clone(),
                    env: env.clone(),
                    domain: domain.clone(),
                }))
                .wrap(
                    ErrorHandlers::new()
                        .handler(StatusCode::INTERNAL_SERVER_ERROR, render_error_page)
                        .handler(StatusCode::NOT_FOUND, render_error_page)
                        .handler(StatusCode::UNAUTHORIZED, render_error_page)
                        .handler(StatusCode::FORBIDDEN, render_error_page),
                );

            if let Some(ref configure_fn) = configure_fn {
                let cf = configure_fn.clone();
                app = app.configure(move |cfg| (cf)(cfg));
            }

            app.service(
                web::scope("/_astro").route(
                    "/{path:.*}",
                    web::get().to(
                        move |path: web::Path<String>, req: actix_web::HttpRequest| {
                            let filename = format!("_astro/{}", path.into_inner());
                            async move {
                                serve_from_dist(dist_dir, &filename, req.method().as_str()).await
                            }
                        },
                    ),
                ),
            )
            .default_service(web::to(move |req: actix_web::HttpRequest| async move {
                let path = req.path().trim_start_matches('/');
                if path.is_empty() {
                    return Ok::<HttpResponse, actix_web::Error>(HttpResponse::NotFound().finish());
                }
                match serve_from_dist(dist_dir, path, req.method().as_str()).await {
                    Ok(res) => Ok(res),
                    Err(_) => Ok(HttpResponse::NotFound().finish()),
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

fn add_templates(tera: &mut Tera, dir: &Dir) {
    for file in dir.files() {
        if let Some(ext) = file.path().extension() {
            if ext == "html" {
                let path = file.path().to_str().unwrap().replace("\\", "/");
                let name = if path == "index.html" {
                    "index".to_string()
                } else if let Some(stripped) = path.strip_suffix("/index.html") {
                    stripped.to_string()
                } else if let Some(stripped) = path.strip_suffix(".html") {
                    stripped.to_string()
                } else {
                    path
                };

                debug!("Registering template: {}", name);
                let content = file.contents_utf8().unwrap();
                tera.add_raw_template(&name, content).unwrap();
            }
        }
    }
    for subd in dir.dirs() {
        add_templates(tera, subd);
    }
}

async fn serve_from_dist(
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
        .body(file.contents().to_vec()))
}

fn render_error_page<B>(res: ServiceResponse<B>) -> actix_web::Result<ErrorHandlerResponse<B>>
where
    B: MessageBody + 'static,
{
    let (req, res) = res.into_parts();
    let data = req.app_data::<web::Data<AppData>>().cloned().unwrap();
    let status = res.status();

    let is_logged_in = crate::auth::read_jwt(&req).is_ok();

    let template = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            if is_logged_in {
                "noauth"
            } else {
                "public_noauth"
            }
        }
        _ => {
            if is_logged_in {
                "error"
            } else {
                "public_error"
            }
        }
    };

    let error_msg = req.extensions().get::<String>().cloned();
    if let Some(ref msg) = error_msg {
        error!("Error [{}]: {}", status, msg);
    }

    let display_error = if data.env == Env::Dev {
        error_msg.unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Unknown Error")
                .to_string()
        })
    } else {
        status
            .canonical_reason()
            .unwrap_or("An unexpected error occurred")
            .to_string()
    };

    Ok(ErrorHandlerResponse::Future(Box::pin(async move {
        let ctx = serde_json::json!({
            "status": status.as_u16(),
            "error": display_error,
        });

        let res_template = data.render_template(template, &ctx).await;
        let mut res = res_template;
        *res.status_mut() = status;

        let res = ServiceResponse::new(req, res).map_into_right_body();

        Ok(res)
    })))
}

fn load_env_file() {
    if std::env::var("ENV").unwrap_or_default() != "prod" {
        dotenv().ok();
        debug!("Running in DEV mode, .env loaded.");
    } else {
        debug!("Running in PROD mode, skip loading .env file.");
    }
}
