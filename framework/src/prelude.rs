//! Prelude for the `my_rust_framework` crate.
//!
//! This module re-exports common types and traits for ease of use.

pub use crate::{
    AppData, Env, FrameworkApp,
    auth::{AdminUser, AuthUser, create_jwt, hash_password, verify_password},
    error::{AppError, AppResult, ResultExt},
    structs::{Table, TableAction, TableHeader, User, UserRole},
};

// Full crate re-exports (so users don't need them in Cargo.toml)
pub use actix_web::{
    self, HttpResponse, Responder, cookie, delete, get, http, http::header::LOCATION, main, post,
    put, web, web::Data, web::Form,
};
pub use include_dir;
pub use log::{self, debug, error, info, warn};
pub use reqwest;
pub use serde::{self, Deserialize, Serialize};
pub use serde_json::{self, json};
pub use tera::{self, Context};
pub use tokio_cron_scheduler;

// Common traits/types
pub use std::convert::{TryFrom, TryInto};
