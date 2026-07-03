use crate::AppData;
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
    #[must_use]
    pub fn user_message(&self) -> String {
        match self {
            Self::Db(_) => "A database error occurred.".into(),
            Self::Reqwest(_) => "Communication with an external service failed.".into(),
            Self::Serde(_) => "Processing data failed.".into(),
            Self::NoAuth => "Access denied.".into(),
            Self::NotFound(msg)
            | Self::Auth(msg)
            | Self::BadRequest(msg)
            | Self::Internal(msg)
            | Self::User(msg) => msg.clone(),
        }
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

    fn error_response(&self) -> HttpResponse {
        log::error!("AppError ({}): {}", self.status_code(), self);
        let mut res = HttpResponse::new(self.status_code());
        res.extensions_mut().insert(self.user_message());
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
}
