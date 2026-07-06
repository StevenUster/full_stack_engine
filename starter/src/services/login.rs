use crate::{
    AppData, AppError, AppResult, AppRole, Data, Deserialize, Env, Form, HttpResponse, LOCATION,
    Responder, Role, cookie::Cookie, cookie::time::Duration, create_jwt, find_one, get,
    hash_password, json, verify_password,
};
use std::sync::OnceLock;

use crate::tables::user::User;

static DUMMY_HASH: OnceLock<String> = OnceLock::new();

#[derive(Deserialize)]
pub struct FormData {
    email: String,
    password: String,
}

#[get("/login")]
pub async fn get(req: actix_web::HttpRequest) -> impl Responder {
    use super::RenderTplExt;
    req.render_tpl("login", &json!({})).await
}

pub async fn post(
    data: Data<AppData>,
    req: actix_web::HttpRequest,
    form: Form<FormData>,
) -> AppResult {
    use super::RenderTplExt;

    let user = find_one!(User, &data.db, email == form.email.as_str()).await?;

    let dummy_hash = DUMMY_HASH.get_or_init(|| {
        hash_password("dummy_password_for_timing_safety").unwrap_or_else(|_| {
            "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQ$i6PrS9n+AdfNf/U7/lH1XQ".to_string()
        })
    });
    let hash = user.as_ref().map_or(dummy_hash.as_str(), |u| &u.password);

    // Always verify against some hash so a missing account takes as long as a
    // wrong password (no timing-based account enumeration).
    let password_ok = verify_password(&form.password, hash);

    let Some(user) = user.filter(|u| password_ok && !u.role.is_none()) else {
        return Ok(req
            .render_tpl("login", &json!({"error": "invalid_credentials"}))
            .await);
    };

    if !user.is_verified {
        return Ok(req
            .render_tpl("login", &json!({"error": "confirm_email"}))
            .await);
    }

    // The framework's JWT layer works on its own `User` shape; the table
    // struct carries a superset of those fields.
    let claims_user = crate::User::<AppRole> {
        id: user.id,
        email: user.email,
        password: user.password,
        role: user.role,
        created_at: user.created_at,
        is_verified: user.is_verified,
        verification_token: user.verification_token,
    };
    let jwt = create_jwt(&claims_user, &data.jwt_secret)
        .map_err(|e| AppError::Internal(format!("JWT creation error: {e}")))?;

    let cookie = Cookie::build("token", jwt)
        .path("/")
        .same_site(actix_web::cookie::SameSite::Strict)
        .secure(data.env != Env::Dev)
        .max_age(Duration::hours(1))
        .http_only(true)
        .finish();

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/"))
        .cookie(cookie)
        .finish())
}
