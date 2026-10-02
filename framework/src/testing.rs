//! Test-only helpers for apps built on this framework. Nothing here is used
//! by the framework itself at runtime — pull these into an app's own
//! `tests/` so mistakes that would otherwise only surface as a request-time
//! 500 fail loudly in `cargo test`/CI instead.

use std::fmt::Write as _;

use tera::Tera;

use crate::config::{Config, CorsConfig, RateLimitConfig};
use crate::themes::{Theme, ThemeStack};

/// A valid [`Config`] for tests, so a test that only cares about one handler
/// doesn't have to spell out every setting — and doesn't break every time a new
/// one is added.
///
/// `jwt_secret` is taken as an argument because tests routinely mint and verify
/// tokens against a known key. It is not length-checked here: the minimum is a
/// *boot* rule (see [`crate::config::MIN_JWT_SECRET_LEN`]), and a test that
/// wants a short key to prove something should be able to use one.
#[must_use]
pub fn config(jwt_secret: &str) -> Config {
    Config {
        env: crate::Env::Prod,
        domain: "localhost".to_string(),
        protocol: "http".to_string(),
        port: 8080,
        database_url: "sqlite::memory:".to_string(),
        migrations_dir: "./migrations".to_string(),
        jwt_secret: secrecy::SecretString::from(jwt_secret.to_string()),
        theme: None,
        smtp: None,
        email_verification_enabled: false,
        rate_limit: RateLimitConfig {
            per_second: 100,
            burst: 500,
        },
        currency: None,
        cors: CorsConfig::Disabled,
    }
}

/// Resolves `themes` exactly like the app's boot does (active theme = the
/// one no other installed theme extends) and parses every template of the
/// resulting stack into a [`Tera`] — child templates over parent ones, each
/// theme's own copies also under `@{theme}/{name}`. Unlike the boot-time
/// loader, which logs and skips a broken template so one bad page doesn't
/// take the whole app down, this returns every failure.
///
/// Meant for a one-line integration test in a consuming app:
///
/// ```ignore
/// #[test]
/// fn all_templates_parse() {
///     full_stack_engine::testing::load_themes(starter::themes()).unwrap();
/// }
/// ```
///
/// so a broken template (invalid Tera syntax, an `extends` of a missing
/// parent template, an escaping bug in a compile-to-Tera pipeline like
/// `fse-ssr`) fails `cargo test`/CI instead of only surfacing as a runtime
/// 500.
///
/// # Errors
///
/// Returns `Err` when the themes don't resolve (missing parent, ambiguous
/// active theme, …) or with one block per broken template — its name,
/// Tera's error, and its full `source()` chain.
pub fn load_themes(themes: impl IntoIterator<Item = Theme>) -> Result<Tera, String> {
    load_themes_localized(themes, "en", None)
}

/// [`load_themes`], with the locale-aware filters (`date`, `currency`, …)
/// bound to `lang` and `currency` instead of the neutral defaults.
///
/// Use this wherever a test renders a real page: `load_themes` formats dates
/// as ISO and amounts without a symbol, which is *not* what the app will
/// serve, and a page snapshot taken that way would not catch a formatting
/// regression.
///
/// ```ignore
/// full_stack_engine::testing::load_themes_localized(app::themes(), "de", Some("EUR"))
/// ```
///
/// # Errors
///
/// As [`load_themes`].
pub fn load_themes_localized(
    themes: impl IntoIterator<Item = Theme>,
    lang: &str,
    currency: Option<&str>,
) -> Result<Tera, String> {
    let stack = theme_stack(themes)?;
    strict_tera(&stack, lang, currency)
}

/// Every template of `stack` parsed into a [`Tera`] with the filters bound
/// to `lang`/`currency`, failing with one block per broken template.
pub(crate) fn strict_tera(
    stack: &ThemeStack,
    lang: &str,
    currency: Option<&str>,
) -> Result<Tera, String> {
    let mut tera = Tera::default();
    tera.autoescape_on(vec![""]);
    crate::filters::register(&mut tera, lang, currency.map(str::to_string));
    let mut errors = Vec::new();
    stack.load_into(&mut tera, &mut |name, err| {
        let mut msg = format!("{name}: {err}");
        let mut source = std::error::Error::source(&err);
        while let Some(cause) = source {
            let _ = write!(msg, "\n  caused by: {cause}");
            source = cause.source();
        }
        errors.push(msg);
    });
    if errors.is_empty() {
        Ok(tera)
    } else {
        Err(errors.join("\n\n"))
    }
}

/// The [`ThemeStack`] `themes` resolve to — for building an `AppData` in
/// tests.
///
/// # Errors
///
/// Returns the resolution error as a string.
pub fn theme_stack(themes: impl IntoIterator<Item = Theme>) -> Result<ThemeStack, String> {
    ThemeStack::resolve(themes.into_iter().collect(), None).map_err(|e| e.to_string())
}

/// A ready-to-use [`AppData`](crate::AppData) for tests, built from `db` and
/// `themes` with everything else defaulted.
///
/// Exists because the alternative — a struct literal naming every field — means
/// every test in every downstream app breaks whenever the framework adds one.
/// Override any field afterwards:
///
/// ```ignore
/// let mut data = full_stack_engine::testing::app_data(pool, my_themes(), "test-secret");
/// data.env = full_stack_engine::Env::Dev;
/// ```
///
/// # Panics
///
/// Panics if `themes` don't resolve (missing parent, ambiguous active theme).
#[must_use]
pub fn app_data(
    db: sqlx::SqlitePool,
    themes: impl IntoIterator<Item = Theme>,
    jwt_secret: &str,
) -> crate::AppData {
    let stack = theme_stack(themes).expect("themes should resolve");
    let tera = stack.tera();
    let cfg = config(jwt_secret);
    crate::AppData {
        tera,
        db,
        env: cfg.env,
        domain: cfg.domain.clone(),
        protocol: cfg.protocol.clone(),
        smtp_from: cfg.mail_from().to_string(),
        email_verification_enabled: cfg.email_verification_enabled,
        context_injector: None,
        locales: std::collections::HashMap::new(),
        locale_selector: crate::i18n::LocaleSelector::default(),
        themes: std::sync::Arc::new(stack),
        config: std::sync::Arc::new(cfg),
    }
}

/// The app under test: the **production** stack (every middleware, route
/// order, error page, CSP, template set) built from the app's own
/// [`FrameworkApp`](crate::FrameworkApp), over a fresh in-memory database
/// with the app's migrations applied. Templates are loaded strictly — a
/// broken template fails the test — and the model setup is checked.
///
/// Expose the builder from the app (`pub fn app() -> FrameworkApp`, with
/// `run()` being `app().run()`), then:
///
/// ```ignore
/// use full_stack_engine::testing::TestApp;
///
/// #[actix_web::test]
/// async fn managers_see_only_their_events() {
///     let app = TestApp::new(my_app::app()).await;
///     let admin = app.user("admin@test.dev", AppRole::Admin).await;
///     let res = app.post("/admin/events/create").as_user(&admin)
///         .form(&[("name", "Spring Run")]).send().await;
///     assert_eq!(res.status, 302);
///     let res = app.get("/admin/events").as_user(&admin).send().await;
///     assert!(res.body.contains("Spring Run"));
/// }
/// ```
pub struct TestApp {
    stack: crate::AppStack,
    /// The test database, for seeding rows and checking what a request
    /// wrote.
    pub db: sqlx::SqlitePool,
}

/// A signed-in user of a [`TestApp`].
#[derive(Clone)]
pub struct TestUser {
    pub id: i64,
    pub email: String,
    cookie: actix_web::cookie::Cookie<'static>,
}

/// The password every [`TestApp::user`] gets, for tests that log in through
/// the real `/login` form.
pub const TEST_PASSWORD: &str = "test-password-123";

/// Key used to sign test sessions (never a production secret).
const TEST_SECRET: &str = "fse-test-secret-0123456789abcdef-0123456789";

impl TestApp {
    /// Builds the app.
    ///
    /// # Panics
    ///
    /// When the model setup, the themes/templates or the migrations are
    /// broken — exactly what a test should fail on.
    pub async fn new(app: crate::FrameworkApp) -> Self {
        Self::try_new(app)
            .await
            .unwrap_or_else(|err| panic!("TestApp: {err}"))
    }

    /// [`TestApp::new`], returning the problem instead of panicking.
    ///
    /// # Errors
    ///
    /// Model setup ([`crate::models::check`]), theme/template, migration
    /// and startup-hook failures.
    pub async fn try_new(mut app: crate::FrameworkApp) -> Result<Self, String> {
        crate::models::check()?;
        // One connection: every `sqlite::memory:` connection is its own
        // database.
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                "sqlite::memory:"
                    .parse::<sqlx::sqlite::SqliteConnectOptions>()
                    .map_err(|e| e.to_string())?
                    .foreign_keys(true),
            )
            .await
            .map_err(|e| e.to_string())?;
        if let Some(migrator) = app.migrator.take() {
            migrator
                .run(&db)
                .await
                .map_err(|e| format!("migrations: {e}"))?;
        }
        app.run_startup_hooks(&db).await;
        let cfg = std::sync::Arc::new(config(TEST_SECRET));
        let stack = app.into_stack(cfg, db.clone(), crate::TemplateMode::Strict)?;
        Ok(Self { stack, db })
    }

    /// A user with `role`, created (or updated) in the `users` table, with
    /// [`TEST_PASSWORD`] as password and a valid session — call it again
    /// after a request changed the user's role, to sign in with the new one.
    ///
    /// # Panics
    ///
    /// When the `users` table lacks the auth columns (`email`, `password`,
    /// `role`, `sessions_valid_after`).
    pub async fn user<R: crate::structs::Role>(&self, email: &str, role: R) -> TestUser {
        // Ids are unique across every TestApp in the process: the session
        // cutoff cache is process-wide and keyed by user id, so two test
        // databases that both had a "user 2" would read each other's
        // revocations.
        static NEXT_ID: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1_000_000);
        let fresh_id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let hash = crate::auth::hash_password(TEST_PASSWORD).expect("argon2 hashing");
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO users (id, email, password, role, sessions_valid_after) \
             VALUES (?, ?, ?, ?, 0) \
             ON CONFLICT(email) DO UPDATE SET role = excluded.role, sessions_valid_after = 0 \
             RETURNING id",
        )
        .bind(fresh_id)
        .bind(email)
        .bind(&hash)
        .bind(role.as_str())
        .fetch_one(&self.db)
        .await
        .expect("TestApp::user needs the auth module's users table");
        // A fresh session even if a role change revoked earlier ones.
        crate::auth::invalidate_session_cache(id);
        let jwt = crate::auth::create_session_jwt(id, &role, TEST_SECRET).expect("signing");
        let cookie = actix_web::cookie::Cookie::new("token", jwt);
        TestUser {
            id,
            email: email.to_string(),
            cookie,
        }
    }

    #[must_use]
    pub fn get(&self, uri: &str) -> TestCall<'_> {
        self.call(actix_web::test::TestRequest::get().uri(uri))
    }

    #[must_use]
    pub fn post(&self, uri: &str) -> TestCall<'_> {
        self.call(actix_web::test::TestRequest::post().uri(uri))
    }

    #[must_use]
    pub fn delete(&self, uri: &str) -> TestCall<'_> {
        self.call(actix_web::test::TestRequest::delete().uri(uri))
    }

    /// Any request; the builder methods of [`TestCall`] add a user, a form
    /// or JSON.
    #[must_use]
    pub fn call(&self, req: actix_web::test::TestRequest) -> TestCall<'_> {
        TestCall {
            app: self,
            req,
            peer: None,
        }
    }
}

/// A distinct client address per request, so the auth routes' per-IP rate
/// limits don't trip across a test's requests.
fn next_peer() -> std::net::SocketAddr {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(1);
    let [_, a, b, c] = N.fetch_add(1, Ordering::Relaxed).to_be_bytes();
    std::net::SocketAddr::from(([10, a, b, c], 40000))
}

/// A request being built against a [`TestApp`].
pub struct TestCall<'a> {
    app: &'a TestApp,
    req: actix_web::test::TestRequest,
    peer: Option<std::net::SocketAddr>,
}

impl TestCall<'_> {
    /// Send it signed in as `user`.
    #[must_use]
    pub fn as_user(mut self, user: &TestUser) -> Self {
        self.req = self.req.cookie(user.cookie.clone());
        self
    }

    /// A urlencoded form body.
    #[must_use]
    pub fn form(mut self, fields: &[(&str, &str)]) -> Self {
        self.req = self.req.set_form(fields);
        self
    }

    /// A JSON body.
    #[must_use]
    pub fn json(mut self, value: &serde_json::Value) -> Self {
        self.req = self.req.set_json(value);
        self
    }

    /// Send it from this client address (every request otherwise gets its
    /// own, so per-IP rate limits only apply when a test asks for them).
    #[must_use]
    pub fn from_ip(mut self, addr: std::net::SocketAddr) -> Self {
        self.peer = Some(addr);
        self
    }

    #[must_use]
    pub fn header(mut self, name: &'static str, value: &str) -> Self {
        self.req = self.req.insert_header((name, value.to_string()));
        self
    }

    /// Run the request through the full app.
    pub async fn send(self) -> TestResponse {
        let service = actix_web::test::init_service(self.app.stack.app()).await;
        let req = self.req.peer_addr(self.peer.unwrap_or_else(next_peer));
        let res = actix_web::test::call_service(&service, req.to_request()).await;
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        let body = actix_web::test::read_body(res).await;
        TestResponse {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }
}

/// What a [`TestCall`] got back.
#[derive(Debug)]
pub struct TestResponse {
    pub status: u16,
    pub headers: actix_web::http::header::HeaderMap,
    pub body: String,
}

impl TestResponse {
    /// The `Location` header of a redirect.
    #[must_use]
    pub fn location(&self) -> Option<&str> {
        self.headers
            .get(actix_web::http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
    }

    /// The trailing id of the redirect target — what a generated create
    /// redirects to (`/admin/posts/42` → 42).
    #[must_use]
    pub fn created_id(&self) -> Option<i64> {
        self.location()?.rsplit('/').next()?.parse().ok()
    }

    /// The body parsed as JSON.
    ///
    /// # Panics
    ///
    /// When the body isn't JSON.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("response is not JSON ({e}): {}", self.body))
    }
}
