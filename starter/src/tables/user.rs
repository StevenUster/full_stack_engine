//! The `users` table. The framework's auth layer depends on the columns
//! listed under `[orm.required_columns]` in `fse.toml` — add columns freely,
//! but don't remove those.

use crate::{AppRole, Table, chrono::NaiveDateTime};

#[derive(Table, Debug, Clone)]
pub struct User {
    pub id: i64,
    #[orm(unique)]
    pub email: String,
    pub password: String,
    /// Stored as TEXT via `as_str()`/`FromStr` — the value set lives in
    /// `define_roles!`, not here.
    #[orm(text, default = "none")]
    pub role: AppRole,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    #[orm(default = true)]
    pub is_verified: bool,
    pub verification_token: Option<String>,
    pub verification_token_expires_at: Option<NaiveDateTime>,
    pub reset_token: Option<String>,
    pub reset_token_expires_at: Option<NaiveDateTime>,
    pub pending_email: Option<String>,
    pub email_change_token: Option<String>,
    pub email_change_token_expires_at: Option<NaiveDateTime>,
    /// Any JWT issued (iat) before this unix timestamp is rejected by the
    /// framework's `AuthUser` extractor, enabling server-side session
    /// revocation (role change, password reset, account deletion). 0 = none.
    #[orm(default = 0)]
    pub sessions_valid_after: i64,
    #[orm(default = now)]
    pub created_at: NaiveDateTime,
}
