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

    #[error("Internal error: {0}")]
    Internal(String),
}

pub type AppResult<T = HttpResponse> = Result<T, AppError>;

impl From<String> for AppError {
    fn from(s: String) -> Self {
        AppError::Internal(s)
    }
}

impl AppError {
    pub fn user_message(&self) -> String {
        match self {
            AppError::Db(_) => "A database error occurred.".to_string(),
            AppError::Reqwest(_) => "Failed to communicate with an external service.".to_string(),
            AppError::Serde(_) => "Failed to process data.".to_string(),
            AppError::NotFound(msg) => msg.clone(),
            AppError::Auth(msg) => msg.clone(),
            AppError::NoAuth => "You do not have permission for this action.".to_string(),
            AppError::Internal(_) => "An internal error occurred.".to_string(),
        }
    }
}

impl ResponseError for AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            AppError::Auth(_) | AppError::NoAuth => StatusCode::UNAUTHORIZED,
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn error_response(&self) -> HttpResponse {
        let status = self.status_code();
        let mut res = HttpResponse::new(status);

        log::error!("Internal Error: {}", self);

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
