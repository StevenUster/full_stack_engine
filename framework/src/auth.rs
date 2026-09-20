use crate::structs::Role;
use actix_web::{
    Error, FromRequest, HttpRequest, HttpResponse, dev::Payload, http::header::LOCATION,
};
use argon2::Config;
use jsonwebtoken::{DecodingKey, EncodingKey, Header, Validation, decode, encode};
use rand::{RngCore, rng};

use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Hashes `password` with Argon2 and a fresh random 16-byte salt.
///
/// # Errors
///
/// Returns [`argon2::Error`] if hashing fails (e.g. invalid parameters).
pub fn hash_password(password: &str) -> Result<String, argon2::Error> {
    let mut salt = vec![0u8; 16];
    rng().fill_bytes(&mut salt);

    let config = Config::default();
    let hash = argon2::hash_encoded(password.as_bytes(), &salt, &config)?;

    Ok(hash)
}

#[must_use]
pub fn verify_password(password: &str, hash: &str) -> bool {
    argon2::verify_encoded(hash, password.as_bytes()).unwrap_or_default()
}

#[derive(Debug, Error)]
pub enum JwtError {
    #[error("JWT_SECRET not set")]
    SecretNotSet,
    #[error("Error calculating expiration time: {0}")]
    ExpirationError(#[from] std::time::SystemTimeError),
    #[error("Error encoding the JWT")]
    JwtEncodingError,
    #[error("Error decoding the JWT")]
    JwtDecodingError,
    #[error("JWT has expired")]
    JwtExpired,
    #[error("Token not found in request")]
    TokenNotFound,
    #[error("Unauthorized access")]
    Unauthorized,
}

#[derive(Debug)]
pub enum AuthError {
    Redirect(HttpResponse),
    Other(Error),
}

impl From<JwtError> for AuthError {
    fn from(err: JwtError) -> Self {
        match err {
            JwtError::TokenNotFound
            | JwtError::JwtExpired
            | JwtError::JwtDecodingError
            | JwtError::Unauthorized => AuthError::Redirect(
                HttpResponse::Found()
                    .append_header((LOCATION, "/login"))
                    .finish(),
            ),
            _ => AuthError::Other(actix_web::error::ErrorInternalServerError(err.to_string())),
        }
    }
}

impl From<AuthError> for Error {
    fn from(err: AuthError) -> Error {
        match err {
            AuthError::Redirect(response) => {
                actix_web::error::InternalError::from_response("Authentication required", response)
                    .into()
            }
            AuthError::Other(err) => err,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug)]
#[serde(bound(deserialize = "R: Role"))]
pub struct Claims<R: Role> {
    pub sub: i64,
    pub role: R,
    /// Expiry (unix seconds).
    pub exp: u64,
    /// Issued-at (unix seconds). Compared against the user's
    /// `sessions_valid_after` column so sessions can be revoked server-side.
    /// Defaults to 0 for tokens minted before this field existed.
    #[serde(default)]
    pub iat: u64,
}

/// Creates a signed JWT for `user`, valid for one hour.
///
/// # Errors
///
/// Returns [`JwtError`] if the system clock is before the unix epoch or the
/// token can't be encoded.
pub fn create_jwt<R: Role>(
    user: &crate::structs::User<R>,
    secret: &str,
) -> Result<String, JwtError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(JwtError::ExpirationError)?
        .as_secs();
    let expiration = now + 3600;

    let claims = Claims {
        sub: user.id,
        role: user.role.clone(),
        exp: expiration,
        iat: now,
    };

    let header = Header::default();
    let encoding_key = EncodingKey::from_secret(secret.as_bytes());

    encode(&header, &claims, &encoding_key).map_err(|_| JwtError::JwtEncodingError)
}

/// Reads and validates the JWT from the request's `token` cookie.
///
/// # Errors
///
/// Returns [`JwtError`] if the cookie is missing or the token is invalid or
/// expired.
pub fn read_jwt<R: Role>(req: &HttpRequest) -> Result<Claims<R>, JwtError> {
    let token = req
        .cookie("token")
        .ok_or(JwtError::TokenNotFound)?
        .value()
        .to_string();

    let data = req
        .app_data::<actix_web::web::Data<crate::AppData>>()
        .ok_or(JwtError::SecretNotSet)?;
    let secret = data.jwt_secret();

    let decoding_key = DecodingKey::from_secret(secret.as_bytes());
    let validation = Validation::new(jsonwebtoken::Algorithm::HS256);

    let token_data =
        decode::<Claims<R>>(&token, &decoding_key, &validation).map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => JwtError::JwtExpired,
            _ => JwtError::JwtDecodingError,
        })?;

    Ok(token_data.claims)
}

/// How long a user's session-revocation cutoff is trusted without re-reading
/// it.
///
/// The window is the cost of the cache: a revoked session stays usable for at
/// most this long. Five seconds keeps "log out everywhere", "password changed"
/// and "role downgraded" effectively immediate from a human's point of view,
/// while collapsing a per-request database round-trip into one read per user
/// per five seconds. A logout also clears the entry outright, so the common
/// case is not delayed at all.
const SESSION_CUTOFF_TTL: Duration = Duration::from_secs(5);

/// Cached `users.sessions_valid_after` values. `None` means "no such user",
/// which is cached too — otherwise a flood of tokens for deleted users would
/// hit the database on every request.
type SessionCache = moka::sync::Cache<i64, Option<i64>>;

fn session_cache() -> &'static SessionCache {
    static CACHE: OnceLock<SessionCache> = OnceLock::new();
    CACHE.get_or_init(|| {
        moka::sync::Cache::builder()
            .max_capacity(10_000)
            .time_to_live(SESSION_CUTOFF_TTL)
            .build()
    })
}

/// Drops a user's cached cutoff so their next request re-reads it.
///
/// # Invariant
///
/// **Every write to `users.sessions_valid_after`, and every deletion of a user,
/// must be followed by this call.** Otherwise the revocation does not take
/// effect until the cache entry expires, and "log out everywhere" silently
/// becomes "log out everywhere within [`SESSION_CUTOFF_TTL`]" — a security
/// guarantee quietly downgraded to a performance trade-off.
///
/// Prefer [`revoke_sessions`], which does both halves and cannot be
/// half-called. This is public for the cases that must bump the column as part
/// of a larger atomic statement (consuming a password-reset token, for
/// instance), where splitting the write would be worse than calling this
/// explicitly.
pub fn invalidate_session_cache(user_id: i64) {
    session_cache().invalidate(&user_id);
}

/// Revokes every outstanding session for `user_id`, effective immediately.
///
/// The single entry point worth using: it stamps `sessions_valid_after` *and*
/// drops the cached cutoff, so there is no way to do one without the other.
///
/// # Errors
///
/// Returns the database error if the update fails. The cache is left alone in
/// that case, which is the safe direction — the old cutoff is still the truth.
pub async fn revoke_sessions(db: &sqlx::SqlitePool, user_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET sessions_valid_after = ? WHERE id = ?")
        .bind(chrono::Utc::now().timestamp())
        .bind(user_id)
        .execute(db)
        .await?;
    invalidate_session_cache(user_id);
    Ok(())
}

/// The user's session cutoff, from cache when fresh.
///
/// # Errors
///
/// Returns the database error if the lookup fails. A failure is *not* cached.
async fn session_cutoff(db: &sqlx::SqlitePool, user_id: i64) -> Result<Option<i64>, sqlx::Error> {
    if let Some(cached) = session_cache().get(&user_id) {
        return Ok(cached);
    }
    let cutoff: Option<i64> =
        sqlx::query_scalar("SELECT sessions_valid_after FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(db)
            .await?;
    session_cache().insert(user_id, cutoff);
    Ok(cutoff)
}

#[derive(Debug)]
pub struct AuthUser<R: Role> {
    pub claims: Claims<R>,
}

impl<R: Role> AuthUser<R> {
    /// # Errors
    ///
    /// Returns [`crate::error::AppError::NoAuth`] if the user is not an admin.
    pub fn require_admin(&self) -> Result<(), crate::error::AppError> {
        if self.claims.role.is_admin() {
            Ok(())
        } else {
            Err(crate::error::AppError::NoAuth)
        }
    }

    /// # Errors
    ///
    /// Returns [`crate::error::AppError::NoAuth`] if the user's role lacks
    /// `permission`.
    pub fn require_permission(&self, permission: &str) -> Result<(), crate::error::AppError> {
        if self.claims.role.has_permission(permission) {
            Ok(())
        } else {
            Err(crate::error::AppError::NoAuth)
        }
    }
}

impl<R: Role> FromRequest for AuthUser<R> {
    type Error = Error;
    type Future = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let req = req.clone();
        Box::pin(async move {
            let claims = read_jwt::<R>(&req)
                .map_err(AuthError::from)
                .map_err(Error::from)?;

            // Server-side session revocation: a token is only accepted if the
            // user still exists and the token was issued at or after the user's
            // `sessions_valid_after` cutoff. Bumping that column (on role change,
            // password reset, ...) or deleting the user therefore invalidates
            // outstanding tokens immediately, instead of waiting for `exp`.
            let data = req
                .app_data::<actix_web::web::Data<crate::AppData>>()
                .ok_or_else(|| Error::from(AuthError::from(JwtError::SecretNotSet)))?;

            // Cached for `SESSION_CUTOFF_TTL`; this used to be a database
            // round-trip on every authenticated request.
            let valid_after = session_cutoff(&data.db, claims.sub)
                .await
                .map_err(actix_web::error::ErrorInternalServerError)?;

            // Strictly greater, not `>=`. Both `iat` and the cutoff are whole
            // seconds, and revocation stamps the cutoff with the current
            // second — so `>=` accepted every token minted during that same
            // second, meaning a stolen token issued in the same second as a
            // password reset survived it. `>` revokes the cutoff second
            // entirely: the cost is that a token minted in that same second is
            // rejected even if it was issued just after the revocation, which
            // fails closed rather than open.
            //
            // A negative cutoff (shouldn't happen, but the column is signed)
            // is treated as "no cutoff".
            match valid_after {
                Some(cutoff) if claims.iat > u64::try_from(cutoff).unwrap_or(0) => {
                    Ok(AuthUser { claims })
                }
                _ => Err(Error::from(AuthError::from(JwtError::Unauthorized))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structs::{DefaultRole, User};
    use actix_web::test::TestRequest;
    use actix_web::web;

    const SECRET: &str = "test-secret";

    fn test_user() -> User<DefaultRole> {
        User {
            id: 42,
            email: "user@example.com".to_string(),
            password: String::new(),
            role: DefaultRole::User,
            created_at: chrono::DateTime::from_timestamp(0, 0).unwrap().naive_utc(),
            is_verified: true,
            verification_token: None,
        }
    }

    fn test_app_data(pool: sqlx::SqlitePool) -> web::Data<crate::AppData> {
        web::Data::new(crate::AppData {
            tera: tera::Tera::default(),
            db: pool,
            env: crate::Env::Prod,
            domain: "localhost".to_string(),
            protocol: "http".to_string(),
            config: std::sync::Arc::new(crate::testing::config(SECRET)),
            smtp_from: String::new(),
            email_verification_enabled: false,
            context_injector: None,
            locales: std::collections::HashMap::new(),
            locale_selector: crate::i18n::LocaleSelector::default(),
            themes: std::sync::Arc::default(),
        })
    }

    async fn memory_pool() -> sqlx::SqlitePool {
        // A single connection, because every `:memory:` connection is its own
        // database.
        sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap()
    }

    #[test]
    fn password_hashing_roundtrip() {
        let hash = hash_password("hunter2!").unwrap();
        assert!(verify_password("hunter2!", &hash));
        assert!(!verify_password("hunter3!", &hash));
        // Each hash gets a fresh random salt.
        assert_ne!(hash, hash_password("hunter2!").unwrap());
    }

    #[test]
    fn verify_password_rejects_garbage_hashes() {
        assert!(!verify_password("whatever", "not-a-real-hash"));
        assert!(!verify_password("whatever", ""));
    }

    #[actix_web::test]
    async fn jwt_roundtrip_and_tampering() {
        let jwt = create_jwt(&test_user(), SECRET).unwrap();
        let data = test_app_data(memory_pool().await);

        let req = TestRequest::default()
            .cookie(actix_web::cookie::Cookie::new("token", jwt.clone()))
            .app_data(data.clone())
            .to_http_request();
        let claims = read_jwt::<DefaultRole>(&req).unwrap();
        assert_eq!(claims.sub, 42);
        assert!(claims.exp > claims.iat);

        // Flipping anything in the token invalidates the signature.
        let mut tampered = jwt.clone();
        tampered.pop();
        let req = TestRequest::default()
            .cookie(actix_web::cookie::Cookie::new("token", tampered))
            .app_data(data.clone())
            .to_http_request();
        assert!(read_jwt::<DefaultRole>(&req).is_err());

        // No cookie at all.
        let req = TestRequest::default().app_data(data).to_http_request();
        assert!(matches!(
            read_jwt::<DefaultRole>(&req),
            Err(JwtError::TokenNotFound)
        ));
    }

    #[actix_web::test]
    async fn jwt_signed_with_other_secret_is_rejected() {
        let jwt = create_jwt(&test_user(), "some-other-secret").unwrap();
        let data = test_app_data(memory_pool().await);

        let req = TestRequest::default()
            .cookie(actix_web::cookie::Cookie::new("token", jwt))
            .app_data(data)
            .to_http_request();
        assert!(read_jwt::<DefaultRole>(&req).is_err());
    }

    #[actix_web::test]
    async fn auth_user_enforces_server_side_session_revocation() {
        use actix_web::FromRequest;

        let pool = memory_pool().await;
        sqlx::query(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, sessions_valid_after INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO users (id, sessions_valid_after) VALUES (42, 0)")
            .execute(&pool)
            .await
            .unwrap();

        let data = test_app_data(pool.clone());
        let jwt = create_jwt(&test_user(), SECRET).unwrap();
        let req = || {
            TestRequest::default()
                .cookie(actix_web::cookie::Cookie::new("token", jwt.clone()))
                .app_data(data.clone())
                .to_http_request()
        };

        // Token issued after the cutoff: accepted.
        let user =
            AuthUser::<DefaultRole>::from_request(&req(), &mut actix_web::dev::Payload::None)
                .await
                .unwrap();
        assert_eq!(user.claims.sub, 42);

        // Bumping the cutoff past the token's iat revokes it — but only once
        // the cached cutoff is dropped, which is the invariant
        // `invalidate_session_cache` documents. Written out here rather than
        // hidden behind `revoke_sessions` because this is the path an app takes
        // when it bumps the column inside a larger statement.
        sqlx::query("UPDATE users SET sessions_valid_after = 9999999999 WHERE id = 42")
            .execute(&pool)
            .await
            .unwrap();
        invalidate_session_cache(42);
        assert!(
            AuthUser::<DefaultRole>::from_request(&req(), &mut actix_web::dev::Payload::None)
                .await
                .is_err()
        );

        // A deleted user's outstanding tokens are rejected too.
        sqlx::query("DELETE FROM users WHERE id = 42")
            .execute(&pool)
            .await
            .unwrap();
        invalidate_session_cache(42);
        assert!(
            AuthUser::<DefaultRole>::from_request(&req(), &mut actix_web::dev::Payload::None)
                .await
                .is_err()
        );
    }

    /// `revoke_sessions` is the pairing that cannot be half-done: one call
    /// both stamps the column and drops the cache, so a caller cannot get the
    /// database right and leave the revocation ineffective.
    #[actix_web::test]
    async fn revoke_sessions_takes_effect_without_a_second_call() {
        use actix_web::FromRequest;

        let pool = memory_pool().await;
        sqlx::query(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, sessions_valid_after INTEGER NOT NULL DEFAULT 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO users (id, sessions_valid_after) VALUES (7, 0)")
            .execute(&pool)
            .await
            .unwrap();

        let data = test_app_data(pool.clone());
        let mut user = test_user();
        user.id = 7;
        let jwt = create_jwt(&user, SECRET).unwrap();
        let req = || {
            TestRequest::default()
                .cookie(actix_web::cookie::Cookie::new("token", jwt.clone()))
                .app_data(data.clone())
                .to_http_request()
        };

        // Accepted, and the cutoff is now cached.
        assert!(
            AuthUser::<DefaultRole>::from_request(&req(), &mut actix_web::dev::Payload::None)
                .await
                .is_ok()
        );

        // One call, and the very next request is rejected — no waiting for the
        // cache to expire. Note the token here was minted in the same second as
        // the revocation: with the old `iat >= cutoff` comparison it would have
        // survived, which is the hole this asserts is closed.
        revoke_sessions(&pool, 7).await.unwrap();
        assert!(
            AuthUser::<DefaultRole>::from_request(&req(), &mut actix_web::dev::Payload::None)
                .await
                .is_err()
        );
    }

    #[test]
    fn require_admin_and_permission_gate_by_role() {
        let admin = AuthUser::<DefaultRole> {
            claims: Claims {
                sub: 1,
                role: DefaultRole::Admin,
                exp: 0,
                iat: 0,
            },
        };
        let user = AuthUser::<DefaultRole> {
            claims: Claims {
                sub: 2,
                role: DefaultRole::User,
                exp: 0,
                iat: 0,
            },
        };

        assert!(admin.require_admin().is_ok());
        assert!(admin.require_permission("anything").is_ok());
        assert!(user.require_admin().is_err());
        assert!(user.require_permission("users.read").is_err());
    }
}
