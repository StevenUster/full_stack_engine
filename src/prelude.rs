//! Prelude for the `my_rust_framework` crate.
//!
//! This module re-exports common types and traits for ease of use.

pub use crate::{
    AppData, Env, FrameworkApp,
    auth::AuthUser,
    error::{AppError, AppResult, ResultExt},
    structs::{Table, TableAction, TableHeader, User},
};

// Full crate re-exports (so users don't need them in Cargo.toml)
pub use actix_web::{self, HttpRequest, HttpResponse, Responder, Scope, web};
pub use include_dir;
pub use log::{self, debug, error, info, warn};
pub use reqwest;
pub use serde::{self, Deserialize, Serialize};
pub use serde_json::{self, json};
pub use sqlx::{self, SqlitePool};
pub use tera::{self, Context};
pub use tokio_cron_scheduler;

// Common traits/types
pub use std::convert::{TryFrom, TryInto};
