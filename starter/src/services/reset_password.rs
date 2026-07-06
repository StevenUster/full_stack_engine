use crate::{
    AppData, AppError, AppResult, Deserialize,
    actix_web::{HttpResponse, get, http::header::LOCATION},
    hash_password, json, update, web,
};

use crate::chrono::NaiveDateTime;
use crate::tables::user::User;

#[derive(Deserialize)]
pub struct ResetPasswordQuery {
    token: Option<String>,
    error: Option<String>,
}

#[get("/reset-password")]
pub async fn get(
    _data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    query: web::Query<ResetPasswordQuery>,
) -> AppResult {
    use super::RenderTplExt;
    let Some(token) = &query.token else {
        return Ok(HttpResponse::SeeOther()
            .append_header((LOCATION, "/"))
            .finish());
    };

    let mut ctx = json!({ "token": token });
    if let Some(error) = &query.error {
        ctx["error"] = json!(error);
    }

    Ok(req.render_tpl("reset-password", &ctx).await)
}

#[derive(Deserialize)]
pub struct ResetPasswordForm {
    token: String,
    password: String,
    repeat_password: String,
}

pub async fn post(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    form: web::Form<ResetPasswordForm>,
) -> AppResult {
    use super::RenderTplExt;
    if form.token.is_empty() {
        return Ok(req
            .render_tpl(
                "reset-password",
                &json!({"error": "invalid_token", "token": form.token}),
            )
            .await);
    }

    if form.password.len() < 8 {
        return Ok(HttpResponse::SeeOther()
            .append_header((
                LOCATION,
                format!(
                    "/reset-password?token={}&error=password_too_short",
                    form.token
                ),
            ))
            .finish());
    }

    if form.password != form.repeat_password {
        return Ok(HttpResponse::SeeOther()
            .append_header((
                LOCATION,
                format!(
                    "/reset-password?token={}&error=passwords_mismatch",
                    form.token
                ),
            ))
            .finish());
    }

    let hashed_password =
        hash_password(&form.password).map_err(|e| AppError::Internal(e.to_string()))?;

    // Consuming the token clears it, and stamps `sessions_valid_after` so any
    // JWTs issued before the reset are invalidated. Expired tokens match no
    // row (`NULL > x` is never true, so a missing expiry can't match either).
    let now = super::now_unix();
    let updated = update!(
        User,
        &data.db,
        reset_token == form.token.as_str() && reset_token_expires_at > super::now();
        password = hashed_password,
        reset_token = None::<String>,
        reset_token_expires_at = None::<NaiveDateTime>,
        sessions_valid_after = now
    )
    .await?;

    if updated == 0 {
        return Ok(HttpResponse::SeeOther()
            .append_header((
                LOCATION,
                format!("/reset-password?token={}&error=invalid_token", form.token),
            ))
            .finish());
    }

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/logout"))
        .finish())
}
