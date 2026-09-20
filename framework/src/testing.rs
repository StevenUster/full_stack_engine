//! Test-only helpers for apps built on this framework. Nothing here is used
//! by the framework itself at runtime — pull these into an app's own
//! `tests/` so mistakes that would otherwise only surface as a request-time
//! 500 fail loudly in `cargo test`/CI instead.

use std::fmt::Write as _;

use tera::Tera;

use crate::config::{Config, RateLimitConfig};
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
    let stack = theme_stack(themes)?;
    let mut tera = Tera::default();
    tera.autoescape_on(vec![""]);
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
