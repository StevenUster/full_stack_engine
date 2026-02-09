// Prelude for the `my_rust_framework` crate.
// This module re-exports common types and traits for ease of use.

pub use crate::{
    AppData, Env, FrameworkApp,
    auth::AuthUser,
    error::{AppError, AppResult, ResultExt},
    structs::{Table, TableAction, TableHeader, User},
};

pub use actix_web::{
    HttpRequest, HttpResponse, Resource, Responder, Scope,
    web::{self, Data, Json, Path, Query, ServiceConfig},
};

pub use serde::{Deserialize, Serialize};

pub use sqlx::{self, SqlitePool};

pub use log::{debug, error, info, warn};

pub use std::convert::{TryFrom, TryInto};
