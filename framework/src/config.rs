//! Typed application configuration, read and validated **once** at boot.
//!
//! # Why this exists
//!
//! Configuration used to be read wherever it happened to be needed: four
//! `env::var(...).expect(...)` calls in `run()`, the SMTP triple inside
//! [`crate::mail::send_mail`], the rate-limit numbers inside
//! [`crate::rate_limiter`], `PORT` at bind time, `MIGRATIONS_DIR` during
//! migration. That had two costs worth removing:
//!
//! 1. **One error per boot.** Each `expect` panicked on the first missing
//!    variable, so a fresh deployment was fixed by rebooting once per mistake.
//!    [`Config::from_env`] collects *every* problem and reports them together.
//! 2. **Failures discovered by users.** SMTP was read at send time, so a typo
//!    in `SMTP_HOST` passed boot, passed every test, and surfaced weeks later
//!    as a password reset that silently did nothing. It is now validated at
//!    boot, and `EMAIL_VERIFICATION_ENABLED=true` without working SMTP is a
//!    boot error rather than a registration flow that strands every new user.
//!
//! # Secrets
//!
//! `JWT_SECRET` and `SMTP_PASS` are [`SecretString`], not `String`: they have
//! no `Debug` or `Display`, so they cannot reach a log line, an error message
//! or a telemetry backend by accident. Reading one is an explicit
//! `.expose_secret()` that shows up in review.

use secrecy::{ExposeSecret, SecretString};

use crate::Env;

/// The shortest `JWT_SECRET` the app will boot with. HS256 signs and verifies
/// with this exact byte string as the key, so its entropy is the only thing
/// standing between an attacker and a forged token; below the 256-bit hash
/// width a secret is brute-forceable offline. `openssl rand -base64 32`
/// produces a 44-char value that clears this comfortably.
pub const MIN_JWT_SECRET_LEN: usize = 32;

const DEFAULT_PORT: u16 = 8080;
const DEFAULT_MIGRATIONS_DIR: &str = "./migrations";
const DEFAULT_RATE_LIMIT_PER_SECOND: u64 = 100;
const DEFAULT_RATE_LIMIT_BURST: u32 = 500;

/// Everything that was wrong with the environment, not just the first thing.
#[derive(Debug)]
pub struct ConfigError {
    problems: Vec<String>,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid configuration ({} problem{}):",
            self.problems.len(),
            if self.problems.len() == 1 { "" } else { "s" }
        )?;
        for problem in &self.problems {
            write!(f, "\n  - {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ConfigError {}

/// SMTP credentials. Present only when all of `SMTP_HOST`, `SMTP_USER` and
/// `SMTP_PASS` are set — a partially configured mailer is a boot error, not a
/// half-working one.
#[derive(Clone)]
pub struct SmtpConfig {
    pub host: String,
    /// Parsed out of `SMTP_HOST`'s `host:port` form, if it carried one.
    pub port: Option<u16>,
    /// Also the `From:` address of every mail the framework sends.
    pub user: String,
    pub password: SecretString,
}

impl std::fmt::Debug for SmtpConfig {
    /// Hand-written so the password cannot be printed even by
    /// `#[derive(Debug)]` on a containing type.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Site-wide per-IP rate limit (see [`crate::rate_limiter`]).
#[derive(Copy, Clone, Debug)]
pub struct RateLimitConfig {
    pub per_second: u64,
    pub burst: u32,
}

/// The whole application's configuration.
#[derive(Clone)]
pub struct Config {
    /// Only an explicit `ENV=dev` opts into dev mode; see [`parse_env`].
    pub env: Env,
    pub domain: String,
    /// `http` or `https` — nothing else parses.
    pub protocol: String,
    pub port: u16,
    pub database_url: String,
    /// Where migrations are read from when the app supplies no embedded
    /// [`sqlx::migrate::Migrator`].
    pub migrations_dir: String,
    pub jwt_secret: SecretString,
    /// `THEME`, which overrides [`crate::FrameworkApp::active_theme`].
    pub theme: Option<String>,
    pub smtp: Option<SmtpConfig>,
    pub email_verification_enabled: bool,
    pub rate_limit: RateLimitConfig,
}

impl std::fmt::Debug for Config {
    /// Hand-written: a derived `Debug` would be a convenient way to dump the
    /// JWT secret into a log line.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("env", &if self.env == Env::Dev { "dev" } else { "prod" })
            .field("domain", &self.domain)
            .field("protocol", &self.protocol)
            .field("port", &self.port)
            .field("database_url", &self.database_url)
            .field("migrations_dir", &self.migrations_dir)
            .field("jwt_secret", &"<redacted>")
            .field("theme", &self.theme)
            .field("smtp", &self.smtp)
            .field(
                "email_verification_enabled",
                &self.email_verification_enabled,
            )
            .field("rate_limit", &self.rate_limit)
            .finish()
    }
}

impl Config {
    /// Reads and validates the process environment.
    ///
    /// Call after the `.env` file has been loaded.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] listing *every* problem found, so one boot
    /// attempt reports all of them.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_source(&|key| std::env::var(key).ok())
    }

    /// The validation itself, over an arbitrary lookup, so the rules are
    /// testable without touching the process environment (which is global and
    /// makes parallel tests race).
    fn from_source(get: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut problems = Vec::new();

        // Empty is treated as unset throughout: `docker compose` passes
        // `FOO=` for every variable the operator left blank.
        let var = |key: &str| {
            get(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let mut required = |key: &str| {
            if let Some(value) = var(key) {
                value
            } else {
                problems.push(format!("{key} is not set"));
                String::new()
            }
        };

        let domain = required("DOMAIN");
        let protocol = required("PROTOCOL");
        let database_url = required("DATABASE_URL");
        let jwt_secret = required("JWT_SECRET");

        if !protocol.is_empty() && protocol != "http" && protocol != "https" {
            problems.push(format!(
                "PROTOCOL is `{protocol}`; expected `http` or `https`"
            ));
        }
        if !jwt_secret.is_empty() && jwt_secret.len() < MIN_JWT_SECRET_LEN {
            problems.push(format!(
                "JWT_SECRET is too short ({} bytes): use at least {MIN_JWT_SECRET_LEN} \
                 (e.g. `openssl rand -base64 32`). A short secret makes HS256 tokens \
                 brute-forceable.",
                jwt_secret.len(),
            ));
        }

        let port = match var("PORT") {
            None => DEFAULT_PORT,
            Some(raw) => match raw.parse::<u16>() {
                Ok(0) | Err(_) => {
                    problems.push(format!("PORT is `{raw}`; expected a number in 1..=65535"));
                    DEFAULT_PORT
                }
                Ok(port) => port,
            },
        };

        let smtp = match parse_smtp(&var) {
            Ok(smtp) => smtp,
            Err(problem) => {
                problems.push(problem);
                None
            }
        };

        let email_verification_enabled =
            var("EMAIL_VERIFICATION_ENABLED").as_deref() == Some("true");
        // The flow this flag turns on cannot work without a mailer: every new
        // account would be created unverified with no way to verify it. Fail
        // the boot instead of stranding users.
        if email_verification_enabled && smtp.is_none() {
            problems.push(
                "EMAIL_VERIFICATION_ENABLED=true but SMTP is not configured: new accounts \
                 could never be verified. Set SMTP_HOST/SMTP_USER/SMTP_PASS, or turn \
                 verification off."
                    .to_string(),
            );
        }

        let rate_limit = RateLimitConfig {
            per_second: parse_positive(
                &var,
                "GLOBAL_RATE_LIMIT_PER_SECOND",
                DEFAULT_RATE_LIMIT_PER_SECOND,
                &mut problems,
            ),
            burst: parse_positive(
                &var,
                "GLOBAL_RATE_LIMIT_BURST",
                DEFAULT_RATE_LIMIT_BURST,
                &mut problems,
            ),
        };

        if !problems.is_empty() {
            return Err(ConfigError { problems });
        }

        Ok(Self {
            env: parse_env(var("ENV").as_deref()),
            domain,
            protocol,
            port,
            database_url,
            migrations_dir: var("MIGRATIONS_DIR")
                .unwrap_or_else(|| DEFAULT_MIGRATIONS_DIR.to_string()),
            jwt_secret: SecretString::from(jwt_secret),
            theme: var("THEME"),
            smtp,
            email_verification_enabled,
            rate_limit,
        })
    }

    /// The site's public base URL, e.g. `https://example.com` — the prefix
    /// every link in an outgoing email needs.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("{}://{}", self.protocol, self.domain)
    }

    /// The `From:` address of framework mail, or `""` when no mailer is
    /// configured.
    #[must_use]
    pub fn mail_from(&self) -> &str {
        self.smtp.as_ref().map_or("", |s| s.user.as_str())
    }

    /// The JWT signing key. Named so that every read is visible in review.
    #[must_use]
    pub fn jwt_secret(&self) -> &str {
        self.jwt_secret.expose_secret()
    }
}

/// All three SMTP variables or none. A half-configured mailer is the failure
/// mode worth catching: it looks fine until someone needs a password reset.
fn parse_smtp(var: &dyn Fn(&str) -> Option<String>) -> Result<Option<SmtpConfig>, String> {
    let host = var("SMTP_HOST");
    let user = var("SMTP_USER");
    let password = var("SMTP_PASS");

    match (host, user, password) {
        (None, None, None) => Ok(None),
        (Some(host), Some(user), Some(password)) => {
            let (host, port) = match host.rsplit_once(':') {
                Some((h, p)) => match p.parse::<u16>() {
                    Ok(port) => (h.to_string(), Some(port)),
                    Err(_) => {
                        return Err(format!("SMTP_HOST has a non-numeric port: `{host}`"));
                    }
                },
                None => (host, None),
            };
            if !user.contains('@') {
                return Err(format!(
                    "SMTP_USER is `{user}`; expected an email address (it is also the \
                     From: address of every mail the app sends)"
                ));
            }
            Ok(Some(SmtpConfig {
                host,
                port,
                user,
                password: SecretString::from(password),
            }))
        }
        (host, user, password) => {
            let mut missing = Vec::new();
            if host.is_none() {
                missing.push("SMTP_HOST");
            }
            if user.is_none() {
                missing.push("SMTP_USER");
            }
            if password.is_none() {
                missing.push("SMTP_PASS");
            }
            Err(format!(
                "SMTP is partially configured: {} missing. Set all three or none.",
                missing.join(", ")
            ))
        }
    }
}

/// A positive integer setting, falling back to `default` when unset and
/// recording a problem when present but unusable.
fn parse_positive<T>(
    var: &dyn Fn(&str) -> Option<String>,
    key: &str,
    default: T,
    problems: &mut Vec<String>,
) -> T
where
    T: std::str::FromStr + PartialEq + From<u8> + Copy,
{
    match var(key) {
        None => default,
        Some(raw) => match raw.parse::<T>() {
            Ok(value) if value != T::from(0u8) => value,
            _ => {
                problems.push(format!("{key} is `{raw}`; expected a positive number"));
                default
            }
        },
    }
}

/// Only an explicit `ENV=dev` opts into dev mode. Anything else — unset,
/// "prod", or a typo like "production" — gets the hardened production
/// behaviour (secure cookies, no dev-server proxy, no detailed error
/// messages), so a misconfiguration fails safe.
#[must_use]
pub fn parse_env(value: Option<&str>) -> Env {
    match value {
        Some("dev") => Env::Dev,
        _ => Env::Prod,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A minimal valid environment; tests override or remove single keys.
    fn base() -> HashMap<String, String> {
        [
            ("DOMAIN", "example.com"),
            ("PROTOCOL", "https"),
            ("DATABASE_URL", "sqlite:./data/db.sqlite"),
            ("JWT_SECRET", "YmFzZTY0LWVuY29kZWQtc2VjcmV0LTMyLWJ5dGVzIQ=="),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
    }

    fn load(vars: &HashMap<String, String>) -> Result<Config, ConfigError> {
        Config::from_source(&|key| vars.get(key).cloned())
    }

    #[test]
    fn a_minimal_environment_loads_with_defaults() {
        let config = load(&base()).unwrap();
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.migrations_dir, "./migrations");
        assert_eq!(config.base_url(), "https://example.com");
        // Unset means prod, so a missing ENV never accidentally loosens
        // cookies or error pages.
        assert!(config.env == Env::Prod);
        assert!(config.smtp.is_none());
        assert!(!config.email_verification_enabled);
        assert_eq!(config.rate_limit.per_second, DEFAULT_RATE_LIMIT_PER_SECOND);
        assert_eq!(config.rate_limit.burst, DEFAULT_RATE_LIMIT_BURST);
    }

    #[test]
    fn every_problem_is_reported_from_one_boot() {
        // The point of the module: an empty environment used to be fixed by
        // rebooting once per missing variable.
        let err = load(&HashMap::new()).unwrap_err();
        let message = err.to_string();
        for key in ["DOMAIN", "PROTOCOL", "DATABASE_URL", "JWT_SECRET"] {
            assert!(message.contains(key), "{key} missing from: {message}");
        }
        assert!(message.contains("4 problems"), "{message}");
    }

    #[test]
    fn a_short_jwt_secret_is_rejected() {
        let mut vars = base();
        vars.insert("JWT_SECRET".into(), "x".repeat(MIN_JWT_SECRET_LEN - 1));
        let message = load(&vars).unwrap_err().to_string();
        assert!(message.contains("JWT_SECRET is too short"), "{message}");
        // Exactly the minimum is fine.
        vars.insert("JWT_SECRET".into(), "x".repeat(MIN_JWT_SECRET_LEN));
        assert!(load(&vars).is_ok());
    }

    #[test]
    fn protocol_and_port_must_be_usable() {
        let mut vars = base();
        vars.insert("PROTOCOL".into(), "htps".into());
        vars.insert("PORT".into(), "not-a-port".into());
        let message = load(&vars).unwrap_err().to_string();
        assert!(message.contains("PROTOCOL is `htps`"), "{message}");
        assert!(message.contains("PORT is `not-a-port`"), "{message}");

        // Port 0 would bind an arbitrary port, which is never intended.
        vars.insert("PROTOCOL".into(), "http".into());
        vars.insert("PORT".into(), "0".into());
        assert!(load(&vars).unwrap_err().to_string().contains("PORT"));
    }

    #[test]
    fn smtp_is_all_three_variables_or_none() {
        let mut vars = base();
        // None: fine, the app simply cannot send mail.
        assert!(load(&vars).unwrap().smtp.is_none());

        // Two of three is the case worth catching — it used to pass boot and
        // fail at the first password reset.
        vars.insert("SMTP_HOST".into(), "smtp.example.com:587".into());
        vars.insert("SMTP_USER".into(), "info@example.com".into());
        let message = load(&vars).unwrap_err().to_string();
        assert!(message.contains("partially configured"), "{message}");
        assert!(message.contains("SMTP_PASS"), "{message}");

        // All three: parsed, with the port split off the host.
        vars.insert("SMTP_PASS".into(), "hunter2".into());
        let smtp = load(&vars).unwrap().smtp.unwrap();
        assert_eq!(smtp.host, "smtp.example.com");
        assert_eq!(smtp.port, Some(587));
        assert_eq!(smtp.user, "info@example.com");
    }

    #[test]
    fn smtp_host_without_a_port_is_allowed_and_a_bad_port_is_not() {
        let mut vars = base();
        vars.insert("SMTP_USER".into(), "info@example.com".into());
        vars.insert("SMTP_PASS".into(), "hunter2".into());

        vars.insert("SMTP_HOST".into(), "smtp.example.com".into());
        let smtp = load(&vars).unwrap().smtp.unwrap();
        assert_eq!(smtp.host, "smtp.example.com");
        assert_eq!(smtp.port, None);

        vars.insert("SMTP_HOST".into(), "smtp.example.com:ohno".into());
        assert!(
            load(&vars)
                .unwrap_err()
                .to_string()
                .contains("non-numeric port")
        );
    }

    #[test]
    fn email_verification_without_a_mailer_fails_the_boot() {
        let mut vars = base();
        vars.insert("EMAIL_VERIFICATION_ENABLED".into(), "true".into());
        // Otherwise every new account is created unverified with no way to
        // ever verify it.
        let message = load(&vars).unwrap_err().to_string();
        assert!(message.contains("EMAIL_VERIFICATION_ENABLED"), "{message}");

        vars.insert("SMTP_HOST".into(), "smtp.example.com:587".into());
        vars.insert("SMTP_USER".into(), "info@example.com".into());
        vars.insert("SMTP_PASS".into(), "hunter2".into());
        let config = load(&vars).unwrap();
        assert!(config.email_verification_enabled);
        assert_eq!(config.mail_from(), "info@example.com");
    }

    #[test]
    fn rate_limits_reject_zero_and_garbage() {
        let mut vars = base();
        vars.insert("GLOBAL_RATE_LIMIT_PER_SECOND".into(), "0".into());
        assert!(
            load(&vars)
                .unwrap_err()
                .to_string()
                .contains("GLOBAL_RATE_LIMIT_PER_SECOND")
        );

        vars.insert("GLOBAL_RATE_LIMIT_PER_SECOND".into(), "50".into());
        vars.insert("GLOBAL_RATE_LIMIT_BURST".into(), "nope".into());
        assert!(
            load(&vars)
                .unwrap_err()
                .to_string()
                .contains("GLOBAL_RATE_LIMIT_BURST")
        );

        vars.insert("GLOBAL_RATE_LIMIT_BURST".into(), "200".into());
        let config = load(&vars).unwrap();
        assert_eq!(config.rate_limit.per_second, 50);
        assert_eq!(config.rate_limit.burst, 200);
    }

    #[test]
    fn blank_values_count_as_unset() {
        // `docker compose` passes `FOO=` for anything the operator left blank.
        let mut vars = base();
        vars.insert("PORT".into(), String::new());
        vars.insert("THEME".into(), "  ".into());
        vars.insert("SMTP_HOST".into(), String::new());
        let config = load(&vars).unwrap();
        assert_eq!(config.port, DEFAULT_PORT);
        assert_eq!(config.theme, None);
        assert!(config.smtp.is_none());
    }

    #[test]
    fn secrets_are_not_printable() {
        let mut vars = base();
        vars.insert("SMTP_HOST".into(), "smtp.example.com:587".into());
        vars.insert("SMTP_USER".into(), "info@example.com".into());
        vars.insert("SMTP_PASS".into(), "super-secret-password".into());
        let config = load(&vars).unwrap();

        // `Debug` is the accident this guards against — it is what a
        // `tracing::error!(?config)` or a derived Debug on a wrapper prints.
        let debug = format!("{config:?}");
        assert!(!debug.contains("super-secret-password"), "{debug}");
        assert!(!debug.contains(config.jwt_secret()), "{debug}");
        assert!(debug.contains("<redacted>"));
        // The values are still reachable, deliberately explicitly.
        assert_eq!(
            config.smtp.as_ref().unwrap().password.expose_secret(),
            "super-secret-password"
        );
    }

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
}
