//! The built-in auth module: login, logout, registration (with optional
//! email verification) and password reset — the flows every app used to
//! copy from the starter, generalized over the app's role enum.
//!
//! Enable it with one builder call:
//!
//! ```ignore
//! FrameworkApp::new(&DIST_DIR)
//!     .module(auth_module::module::<AppRole>())
//! ```
//!
//! Everything is overridable like any module: an app route on the same path
//! wins, and the pages render the `login` / `register` / `register-success`
//! / `forgot-password` / `reset-password` templates plus the
//! `emails/verify` and `emails/password-reset` email templates — the default
//! theme provides them, an app page of the same name replaces one.
//!
//! ## The users-table contract
//!
//! The module speaks to the `users` table through the `[orm.required_columns]`
//! contract (bound-parameter SQL, no string interpolation): `id`, `email`,
//! `password`, `role`, `first_name`, `last_name`, `is_verified`,
//! `verification_token`, `verification_token_expires_at`, `reset_token`,
//! `reset_token_expires_at`, `sessions_valid_after`, `created_at`. Apps add
//! columns freely (extra NOT NULL columns need defaults so registration can
//! insert).
//!
//! Self-registered accounts get the role named `"user"`
//! (`R::from_role_str("user")` decides what that means for the app).

use std::sync::OnceLock;

use actix_web::cookie::time::Duration;
use actix_web::cookie::{Cookie, SameSite};
use actix_web::http::header::LOCATION;
use actix_web::{HttpRequest, HttpResponse, web};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{create_jwt, hash_password, verify_password};
use crate::error::{AppError, AppResult};
use crate::modules::ModuleDef;
use crate::rate_limiter::{auth_rate_limiter, custom_rate_limiter};
use crate::structs::{Role, User};
use crate::{AppData, Env, RenderTplExt};

/// Role name assigned to self-registered accounts.
const DEFAULT_ROLE: &str = "user";
/// How long a single-use email token (verification, password reset) lives.
const TOKEN_TTL_HOURS: i64 = 24;

/// The auth module — pass to [`crate::FrameworkApp::module`]. `R` is the
/// app's role enum from `define_roles!`.
#[must_use]
pub fn module<R: Role>() -> ModuleDef {
    ModuleDef::new("auth").routes(mount::<R>)
}

fn mount<R: Role>(cfg: &mut web::ServiceConfig) {
    cfg.route("/login", web::get().to(login_form));
    cfg.service(
        web::resource("/login")
            .route(web::post().to(login_submit::<R>))
            .wrap(auth_rate_limiter()),
    );
    cfg.route("/logout", web::get().to(logout));
    cfg.route("/logout", web::post().to(logout));

    cfg.route("/register", web::get().to(register_form));
    cfg.service(
        web::resource("/register")
            .route(web::post().to(register_submit))
            .wrap(auth_rate_limiter()),
    );
    cfg.route("/register-success", web::get().to(register_success));
    cfg.route("/verify-email", web::get().to(verify_email));

    cfg.route("/forgot-password", web::get().to(forgot_password_form));
    cfg.service(
        // Sends an email to a caller-chosen address: a small burst to allow
        // correcting a typo, then effectively one per hour per IP.
        web::resource("/forgot-password")
            .route(web::post().to(forgot_password_submit))
            .wrap(custom_rate_limiter(3600, 1)),
    );
    cfg.route("/reset-password", web::get().to(reset_password_form));
    cfg.service(
        web::resource("/reset-password")
            .route(web::post().to(reset_password_submit))
            .wrap(auth_rate_limiter()),
    );
}

// ------------------------------------------------------------------- rows

/// The contract columns login needs, independent of the app's table struct.
#[derive(sqlx::FromRow)]
struct AuthRow {
    id: i64,
    email: String,
    password: String,
    role: String,
    is_verified: bool,
    created_at: chrono::NaiveDateTime,
}

async fn fetch_by_email(db: &sqlx::SqlitePool, email: &str) -> sqlx::Result<Option<AuthRow>> {
    sqlx::query_as::<_, AuthRow>(
        "SELECT id, email, password, role, is_verified, created_at \
         FROM users WHERE email = ?",
    )
    .bind(email)
    .fetch_optional(db)
    .await
}

fn token_expiry() -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc() + chrono::Duration::hours(TOKEN_TTL_HOURS)
}

fn now() -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

fn session_cookie(data: &AppData, jwt: String) -> Cookie<'static> {
    Cookie::build("token", jwt)
        .path("/")
        .same_site(SameSite::Strict)
        .secure(data.env != Env::Dev)
        .max_age(Duration::hours(1))
        .http_only(true)
        .finish()
}

fn see_other(location: &str) -> HttpResponse {
    HttpResponse::SeeOther()
        .append_header((LOCATION, location.to_string()))
        .finish()
}

// ------------------------------------------------------------------ login

async fn login_form(req: HttpRequest) -> HttpResponse {
    req.render_tpl("login", &json!({})).await
}

#[derive(Deserialize)]
struct LoginForm {
    email: String,
    password: String,
}

static DUMMY_HASH: OnceLock<String> = OnceLock::new();

async fn login_submit<R: Role>(
    data: web::Data<AppData>,
    req: HttpRequest,
    form: web::Form<LoginForm>,
) -> AppResult {
    let user = fetch_by_email(&data.db, form.email.trim()).await?;

    let dummy_hash = DUMMY_HASH.get_or_init(|| {
        hash_password("dummy_password_for_timing_safety").unwrap_or_else(|_| {
            "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQ$i6PrS9n+AdfNf/U7/lH1XQ".to_string()
        })
    });
    let hash = user.as_ref().map_or(dummy_hash.as_str(), |u| &u.password);

    // Always verify against some hash so a missing account takes as long as
    // a wrong password (no timing-based account enumeration).
    let password_ok = verify_password(&form.password, hash);

    let Some(user) =
        user.filter(|u| password_ok && !R::from_role_str(&u.role).is_none())
    else {
        return Ok(req
            .render_tpl("login", &json!({"error": "invalid_credentials"}))
            .await);
    };

    if !user.is_verified {
        return Ok(req
            .render_tpl("login", &json!({"error": "confirm_email"}))
            .await);
    }

    let claims_user = User::<R> {
        id: user.id,
        email: user.email,
        password: user.password,
        role: R::from_role_str(&user.role),
        created_at: user.created_at,
        is_verified: user.is_verified,
        verification_token: None,
    };
    let jwt = create_jwt(&claims_user, &data.jwt_secret)
        .map_err(|e| AppError::Internal(format!("JWT creation error: {e}")))?;

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/"))
        .cookie(session_cookie(&data, jwt))
        .finish())
}

async fn logout(data: web::Data<AppData>) -> HttpResponse {
    let cookie = Cookie::build("token", "")
        .path("/")
        .same_site(SameSite::Strict)
        .secure(data.env != Env::Dev)
        .http_only(true)
        .max_age(Duration::seconds(0))
        .finish();

    HttpResponse::SeeOther()
        .append_header((LOCATION, "/login"))
        .cookie(cookie)
        .finish()
}

// --------------------------------------------------------------- register

async fn register_form(req: HttpRequest) -> HttpResponse {
    req.render_tpl("register", &json!({})).await
}

async fn register_success(req: HttpRequest) -> HttpResponse {
    req.render_tpl("register-success", &json!({})).await
}

#[derive(Deserialize)]
struct RegisterForm {
    first_name: String,
    last_name: String,
    email: String,
    password: String,
    repeat_password: String,
}

async fn register_submit(
    data: web::Data<AppData>,
    req: HttpRequest,
    form: web::Form<RegisterForm>,
) -> AppResult {
    let first_name = form.first_name.trim().to_string();
    let last_name = form.last_name.trim().to_string();
    let email = form.email.trim().to_lowercase();

    // Values echoed back into the form so the user doesn't retype them.
    let render_error = |error: &str| {
        json!({
            "first_name": first_name,
            "last_name": last_name,
            "email": form.email,
            "error": error,
        })
    };

    if first_name.is_empty() || last_name.is_empty() {
        return Ok(req.render_tpl("register", &render_error("missing_name")).await);
    }
    if form.password.len() < 8 {
        return Ok(req
            .render_tpl("register", &render_error("password_too_short"))
            .await);
    }
    if form.password != form.repeat_password {
        return Ok(req
            .render_tpl("register", &render_error("passwords_mismatch"))
            .await);
    }
    if email.is_empty() || !email.contains('@') {
        return Ok(req.render_tpl("register", &render_error("invalid_email")).await);
    }

    let hashed_password =
        hash_password(&form.password).map_err(|e| AppError::Internal(e.to_string()))?;

    let is_verified = !data.email_verification_enabled;
    let verification_token = data
        .email_verification_enabled
        .then(|| uuid::Uuid::new_v4().to_string());
    let expires_at = verification_token.as_ref().map(|_| token_expiry());

    let insert_result = sqlx::query(
        "INSERT INTO users \
         (email, password, role, first_name, last_name, is_verified, \
          verification_token, verification_token_expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&email)
    .bind(&hashed_password)
    .bind(DEFAULT_ROLE)
    .bind(&first_name)
    .bind(&last_name)
    .bind(is_verified)
    .bind(&verification_token)
    .bind(expires_at)
    .execute(&data.db)
    .await;

    match insert_result {
        Ok(_) => {}
        // The email is already registered. Relying on the UNIQUE constraint
        // (instead of a check-then-insert, which races with a concurrent
        // registration) and responding exactly like a successful
        // registration keeps this endpoint from enumerating accounts.
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
            let location = if data.email_verification_enabled {
                "/register-success"
            } else {
                "/login"
            };
            return Ok(see_other(location));
        }
        Err(e) => return Err(AppError::Internal(e.to_string())),
    }

    if data.email_verification_enabled
        && let Some(token) = verification_token
    {
        let verify_url = format!(
            "{}://{}/verify-email?token={token}",
            data.protocol, data.domain
        );
        send_token_email(
            &data,
            &req,
            &email,
            "emails/verify",
            "verify_email",
            &json!({ "verify_url": verify_url }),
        )
        .await?;
        return Ok(see_other("/register-success"));
    }

    Ok(see_other("/login"))
}

/// Renders one of the auth email templates in the request's language and
/// sends it in the background (mail-server hiccups are logged, never shown
/// to the requester).
async fn send_token_email(
    data: &web::Data<AppData>,
    req: &HttpRequest,
    to: &str,
    template: &str,
    locale_section: &str,
    extra: &serde_json::Value,
) -> Result<(), AppError> {
    let t = data.locale(&data.request_lang(req));
    let mut ctx = json!({
        "t": t,
        "base_url": format!("{}://{}", data.protocol, data.domain),
    });
    if let (Some(obj), Some(extra_obj)) = (ctx.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_obj {
            obj.insert(k.clone(), v.clone());
        }
    }

    let body = data.render_email(template, &ctx).await.map_err(|e| {
        log::error!("Failed to render {template}: {e}");
        AppError::Internal("Failed to render email template".to_string())
    })?;

    let subject = t[locale_section]["subject"]
        .as_str()
        .unwrap_or("Notification")
        .to_string();
    let to = to.to_string();
    actix_web::rt::spawn(async move {
        if let Err(e) = crate::mail::send_mail(&to, &subject, &body).await {
            log::error!("Failed to send {subject} email to {to}: {e}");
        }
    });
    Ok(())
}

async fn verify_email(
    data: web::Data<AppData>,
    req: HttpRequest,
    query: web::Query<std::collections::HashMap<String, String>>,
) -> AppResult {
    let Some(token) = query.get("token") else {
        return Ok(HttpResponse::BadRequest().body("Missing token"));
    };

    // An expired token matches no row (`NULL > x` is never true, so a
    // missing expiry can't verify either).
    let updated = sqlx::query(
        "UPDATE users SET is_verified = 1, verification_token = NULL, \
         verification_token_expires_at = NULL \
         WHERE verification_token = ? AND verification_token_expires_at > ?",
    )
    .bind(token)
    .bind(now())
    .execute(&data.db)
    .await?
    .rows_affected();

    if updated == 0 {
        return Ok(req
            .render_tpl("login", &json!({"error": "invalid_token"}))
            .await);
    }
    Ok(req
        .render_tpl("login", &json!({"success": "email_confirmed"}))
        .await)
}

// ---------------------------------------------------------- password reset

async fn forgot_password_form(req: HttpRequest) -> HttpResponse {
    req.render_tpl("forgot-password", &json!({})).await
}

#[derive(Deserialize)]
struct ForgotPasswordForm {
    email: String,
}

async fn forgot_password_submit(
    data: web::Data<AppData>,
    req: HttpRequest,
    form: web::Form<ForgotPasswordForm>,
) -> AppResult {
    let success_ctx = json!({"success": "password_reset_sent"});

    // Unknown addresses get the exact same response — no enumeration.
    let Some(user) = fetch_by_email(&data.db, form.email.trim()).await? else {
        return Ok(req.render_tpl("forgot-password", &success_ctx).await);
    };

    let token = uuid::Uuid::new_v4().to_string();
    sqlx::query("UPDATE users SET reset_token = ?, reset_token_expires_at = ? WHERE id = ?")
        .bind(&token)
        .bind(token_expiry())
        .bind(user.id)
        .execute(&data.db)
        .await?;

    let reset_url = format!(
        "{}://{}/reset-password?token={token}",
        data.protocol, data.domain
    );
    if send_token_email(
        &data,
        &req,
        &user.email,
        "emails/password-reset",
        "password_reset_email",
        &json!({ "reset_url": reset_url }),
    )
    .await
    .is_err()
    {
        return Ok(req
            .render_tpl("forgot-password", &json!({"error": "send_email_failed"}))
            .await);
    }

    Ok(req.render_tpl("forgot-password", &success_ctx).await)
}

#[derive(Deserialize)]
struct ResetPasswordQuery {
    token: Option<String>,
    error: Option<String>,
}

async fn reset_password_form(
    req: HttpRequest,
    query: web::Query<ResetPasswordQuery>,
) -> AppResult {
    let Some(token) = &query.token else {
        return Ok(see_other("/"));
    };
    let mut ctx = json!({ "token": token });
    if let Some(error) = &query.error {
        ctx["error"] = json!(error);
    }
    Ok(req.render_tpl("reset-password", &ctx).await)
}

#[derive(Deserialize)]
struct ResetPasswordForm {
    token: String,
    password: String,
    repeat_password: String,
}

async fn reset_password_submit(
    data: web::Data<AppData>,
    req: HttpRequest,
    form: web::Form<ResetPasswordForm>,
) -> AppResult {
    if form.token.is_empty() {
        return Ok(req
            .render_tpl(
                "reset-password",
                &json!({"error": "invalid_token", "token": form.token}),
            )
            .await);
    }
    if form.password.len() < 8 {
        return Ok(see_other(&format!(
            "/reset-password?token={}&error=password_too_short",
            form.token
        )));
    }
    if form.password != form.repeat_password {
        return Ok(see_other(&format!(
            "/reset-password?token={}&error=passwords_mismatch",
            form.token
        )));
    }

    let hashed_password =
        hash_password(&form.password).map_err(|e| AppError::Internal(e.to_string()))?;

    // Consuming the token clears it, and stamps `sessions_valid_after` so
    // any JWTs issued before the reset are invalidated. Expired tokens match
    // no row.
    let updated = sqlx::query(
        "UPDATE users SET password = ?, reset_token = NULL, \
         reset_token_expires_at = NULL, sessions_valid_after = ? \
         WHERE reset_token = ? AND reset_token_expires_at > ?",
    )
    .bind(&hashed_password)
    .bind(chrono::Utc::now().timestamp())
    .bind(&form.token)
    .bind(now())
    .execute(&data.db)
    .await?
    .rows_affected();

    if updated == 0 {
        return Ok(see_other(&format!(
            "/reset-password?token={}&error=invalid_token",
            form.token
        )));
    }
    Ok(see_other("/logout"))
}
