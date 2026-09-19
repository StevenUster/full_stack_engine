//! The application error type and how it becomes an HTTP response.
//!
//! # Two kinds of error, one type
//!
//! Every failure a handler can return is an [`AppError`], but the variants
//! split into two groups that deserve very different treatment:
//!
//! **Domain errors** — [`AppError::NotFound`], [`Auth`](AppError::Auth),
//! [`NoAuth`](AppError::NoAuth), [`BadRequest`](AppError::BadRequest),
//! [`User`](AppError::User). Someone asked for something they may not have or
//! that does not exist. These are ordinary traffic: they become a 4xx, their
//! message is written for the person reading it, and they are logged at
//! `debug`/`info`. A 404 is not an incident.
//!
//! **Internal errors** — [`Db`](AppError::Db),
//! [`Reqwest`](AppError::Reqwest), [`Serde`](AppError::Serde),
//! [`Internal`](AppError::Internal), [`Unexpected`](AppError::Unexpected).
//! Something the app depends on broke. These become a 500, the user is told
//! nothing about the internals, and the full cause chain is logged at `error`
//! and reported to whatever error backend is configured.
//!
//! [`AppError::is_client_error`] is that split, and it is what decides the log
//! severity — not the call site, which would get it wrong sooner or later.
//!
//! # Why not `anyhow`?
//!
//! Because the error type has a job `anyhow::Error` cannot do: it implements
//! actix's [`ResponseError`], which means each variant must map to a status
//! code and a user-safe message. `thiserror` was already the convention here
//! and it stays. What `anyhow` is genuinely good at — attaching context
//! without losing the cause — is covered by [`AppError::Unexpected`] and the
//! [`ErrorContext`] trait, which keep the `source()` chain intact so
//! [`crate::observability::source_chain`] can render all of it.
//!
//! # How a failure reaches the logs
//!
//! ```text
//!   handler returns Err(AppError)
//!        │
//!        ▼
//!   ResponseError::error_response()   attaches an `ErrorDetail`
//!        │                            (the full cause chain, log-only)
//!        ▼
//!   ErrorHandlers → render_error_page()   renders the themed page,
//!        │                                 carrying the detail forward
//!        ▼
//!   FseRootSpan::on_request_end()     logs it once, at a severity taken
//!                                      from the status, and records it on
//!                                      the request's span
//! ```
//!
//! Note what does *not* happen: no call site logs the error itself. One
//! failure produces one log record, with its whole cause chain, and no
//! handler has to remember to write it.

use crate::AppData;
use crate::observability::{ErrorDetail, source_chain};
use actix_web::{HttpResponse, ResponseError, http::StatusCode, web};
use serde::Serialize;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AppError {
    #[error("Database error: {0}")]
    Db(#[from] sqlx::Error),

    #[error("Request error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("Not Found: {0}")]
    NotFound(String),

    #[error("Unauthorized: {0}")]
    Auth(String),

    #[error("Permission denied")]
    NoAuth,

    #[error("Bad request: {0}")]
    BadRequest(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("{0}")]
    User(String),

    /// An operation failed for a reason the caller wants to describe, without
    /// throwing away what actually went wrong.
    ///
    /// Prefer this over `Internal(format!("... {e}"))`: the cause stays
    /// reachable through `source()`, so
    /// [`source_chain`](crate::observability::source_chain) can render the
    /// whole chain and a telemetry backend can group by the root cause rather
    /// than by a string that differs on every call site. Built through
    /// [`ErrorContext::context`] rather than by hand:
    ///
    /// ```ignore
    /// let raw = std::fs::read(path).context("reading the import file")?;
    /// // logged as: reading the import file: No such file or directory (os error 2)
    /// ```
    #[error("{context}")]
    Unexpected {
        context: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

pub type AppResult<T = HttpResponse> = Result<T, AppError>;

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError::Internal(s)
    }
}

impl From<&str> for AppError {
    fn from(s: &str) -> Self {
        AppError::Internal(s.to_string())
    }
}

impl AppError {
    /// The message to show the person who made the request. Internal variants
    /// deliberately return a fixed sentence: a driver error, a URL or a query
    /// fragment in a response body is an information leak.
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::Db(_) => "A database error occurred.".into(),
            Self::Reqwest(_) => "Communication with an external service failed.".into(),
            Self::Serde(_) => "Processing data failed.".into(),
            Self::NoAuth => "Access denied.".into(),
            Self::Unexpected { .. } => "An internal error occurred.".into(),
            Self::NotFound(msg)
            | Self::Auth(msg)
            | Self::BadRequest(msg)
            | Self::Internal(msg)
            | Self::User(msg) => msg.clone(),
        }
    }

    /// `true` for the domain errors described in the [module docs](self):
    /// something about the *request* was wrong, and the server is healthy.
    ///
    /// Drives log severity and whether the request's span is marked failed, so
    /// a spike of 404s from a scanner never looks like a spike of outages.
    #[must_use]
    pub fn is_client_error(&self) -> bool {
        self.status_code().is_client_error()
    }

    /// What the observability stack reports for this error: the full cause
    /// chain plus the status asked for. The user-facing half is
    /// [`AppError::user_message`], and the two are deliberately separate — see
    /// [`ErrorDetail`].
    #[must_use]
    pub fn detail(&self) -> ErrorDetail {
        ErrorDetail {
            log_message: source_chain(self),
            status: self.status_code(),
        }
    }
}

/// Attaches a description to a failure while keeping the failure itself.
///
/// The ergonomic replacement for `map_err(|e| AppError::Internal(format!("…:
/// {e}")))`, which reads the same at the call site but flattens the cause into
/// a string — losing the `source()` chain that makes an error diagnosable and
/// groupable.
///
/// ```ignore
/// use full_stack_engine::prelude::*;
///
/// let config = std::fs::read_to_string(path).context("reading fse.toml")?;
/// let parsed: Config = serde_json::from_str(&config)
///     .with_context(|| format!("parsing {path}"))?;
/// ```
pub trait ErrorContext<T> {
    /// Wraps the error with a fixed description.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Unexpected`] carrying the original error as its
    /// `source()`.
    fn context(self, context: impl Into<String>) -> Result<T, AppError>;

    /// Wraps the error with a description built only if there is an error —
    /// for contexts that cost something to format.
    ///
    /// # Errors
    ///
    /// Returns [`AppError::Unexpected`] carrying the original error as its
    /// `source()`.
    fn with_context<C, F>(self, context: F) -> Result<T, AppError>
    where
        C: Into<String>,
        F: FnOnce() -> C;
}

impl<T, E> ErrorContext<T> for Result<T, E>
where
    E: std::error::Error + Send + Sync + 'static,
{
    fn context(self, context: impl Into<String>) -> Result<T, AppError> {
        self.map_err(|source| AppError::Unexpected {
            context: context.into(),
            source: Box::new(source),
        })
    }

    fn with_context<C, F>(self, context: F) -> Result<T, AppError>
    where
        C: Into<String>,
        F: FnOnce() -> C,
    {
        self.map_err(|source| AppError::Unexpected {
            context: context().into(),
            source: Box::new(source),
        })
    }
}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            Self::Auth(_) | Self::NoAuth => StatusCode::UNAUTHORIZED,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Builds the response and attaches the [`ErrorDetail`] the rest of the
    /// pipeline reports from.
    ///
    /// Deliberately does **not** log: doing it here logged every 404 at
    /// `error`, and logged it again once the request span started reporting
    /// failures. [`crate::observability::FseRootSpan`] is the one place that
    /// logs a failed request, which is also the only place that knows the
    /// status finally sent.
    fn error_response(&self) -> HttpResponse {
        let mut res = HttpResponse::new(self.status_code());
        res.extensions_mut().insert(self.detail());
        res
    }
}

pub trait ResultExt<T> {
    #[allow(async_fn_in_trait)]
    async fn render(self, data: &web::Data<AppData>, template: &str) -> HttpResponse;
}

impl<T: Serialize> ResultExt<T> for Result<T, AppError> {
    async fn render(self, data: &web::Data<AppData>, template: &str) -> HttpResponse {
        match self {
            Ok(ctx) => data.render_template(template, &ctx).await,
            Err(e) => e.error_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_codes_map_by_variant() {
        assert_eq!(
            AppError::Auth("x".into()).status_code(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(AppError::NoAuth.status_code(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            AppError::NotFound("x".into()).status_code(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            AppError::BadRequest("x".into()).status_code(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AppError::Internal("x".into()).status_code(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[test]
    fn database_details_are_hidden_from_users() {
        // The user-facing message must not leak driver/query details.
        let err = AppError::Db(sqlx::Error::RowNotFound);
        assert_eq!(err.user_message(), "A database error occurred.");
    }

    #[test]
    fn domain_and_internal_errors_are_distinguishable() {
        // Domain errors: the request was wrong, the server is fine.
        for err in [
            AppError::NotFound("x".into()),
            AppError::Auth("x".into()),
            AppError::NoAuth,
            AppError::BadRequest("x".into()),
        ] {
            assert!(err.is_client_error(), "{err} should be a client error");
        }
        // Internal errors: the server is not fine.
        for err in [
            AppError::Db(sqlx::Error::RowNotFound),
            AppError::Internal("x".into()),
            AppError::User("x".into()),
            AppError::Unexpected {
                context: "x".into(),
                source: Box::new(std::io::Error::other("y")),
            },
        ] {
            assert!(!err.is_client_error(), "{err} should be an internal error");
        }
    }

    #[test]
    fn context_preserves_the_cause_chain() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "no such file");
        let err: AppError = Err::<(), _>(io)
            .context("reading the import file")
            .unwrap_err();

        // The message the caller wrote...
        assert_eq!(err.to_string(), "reading the import file");
        // ...with the original error still reachable...
        assert!(std::error::Error::source(&err).is_some());
        // ...and both rendered together for the log.
        assert_eq!(
            err.detail().log_message,
            "reading the import file: no such file"
        );
        // The user is told nothing about the filesystem.
        assert_eq!(err.user_message(), "An internal error occurred.");
    }

    #[test]
    fn with_context_only_formats_on_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        let make = || {
            CALLS.fetch_add(1, Ordering::SeqCst);
            "context"
        };

        assert!(Ok::<_, std::io::Error>(1).with_context(make).is_ok());
        assert_eq!(CALLS.load(Ordering::SeqCst), 0, "not formatted on success");

        assert!(
            Err::<(), _>(std::io::Error::other("boom"))
                .with_context(make)
                .is_err()
        );
        assert_eq!(CALLS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn error_response_attaches_the_detail_for_the_pipeline_to_report() {
        let err = AppError::Db(sqlx::Error::RowNotFound);
        let res = err.error_response();
        assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let extensions = res.extensions();
        let detail = extensions
            .get::<ErrorDetail>()
            .expect("error_response must attach an ErrorDetail");
        // The log side keeps the driver's own words...
        assert!(detail.log_message.contains("no rows returned"));
        assert_eq!(detail.status, StatusCode::INTERNAL_SERVER_ERROR);
        // ...while the body the user gets says none of it.
        assert_eq!(
            AppError::Db(sqlx::Error::RowNotFound).user_message(),
            "A database error occurred."
        );
    }

    #[test]
    fn detail_of_a_domain_error_is_safe_to_show() {
        let err = AppError::NotFound("Product 12 does not exist".into());
        // A domain error's message was written for the user, so there is
        // nothing to hide from them...
        assert_eq!(err.user_message(), "Product 12 does not exist");
        // ...and the log records the same thing, with the variant for context.
        let detail = err.detail();
        assert_eq!(detail.log_message, "Not Found: Product 12 does not exist");
        assert_eq!(detail.status, StatusCode::NOT_FOUND);
    }
}
