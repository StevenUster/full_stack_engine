use crate::{
    AppData, AppError, AppResult, Deserialize,
    actix_web::{HttpResponse, get, http::header::LOCATION},
    hash_password, json, web,
};

#[derive(Deserialize)]
pub struct ResetPasswordQuery {
    token: Option<String>,
}

#[get("/reset-password")]
pub async fn get(data: web::Data<AppData>, query: web::Query<ResetPasswordQuery>) -> AppResult {
    let Some(token) = &query.token else {
        return Ok(HttpResponse::SeeOther()
            .append_header((LOCATION, "/"))
            .finish());
    };

    Ok(data
        .render_tpl("reset-password", &json!({ "token": token }))
        .await)
}

#[derive(Deserialize)]
pub struct ResetPasswordForm {
    token: String,
    password: String,
    repeat_password: String,
}

pub async fn post(data: web::Data<AppData>, form: web::Form<ResetPasswordForm>) -> AppResult {
    if form.token.is_empty() {
        return Ok(data
            .render_tpl(
                "reset-password",
                &json!({"error": "Invalid token", "token": form.token}),
            )
            .await);
    }

    if form.password.len() < 8 {
        return Ok(data
            .render_tpl("reset-password", &json!({"error": "Password must be at least 8 characters long", "token": form.token}))
            .await);
    }

    if form.password != form.repeat_password {
        return Ok(data
            .render_tpl(
                "reset-password",
                &json!({"error": "Passwords do not match", "token": form.token}),
            )
            .await);
    }

    let hashed_password =
        hash_password(&form.password).map_err(|e| AppError::Internal(e.to_string()))?;

    let result = sqlx::query!(
        "UPDATE users SET password = ?, reset_token = NULL WHERE reset_token = ?",
        hashed_password,
        form.token
    )
    .execute(&data.db)
    .await
    .map_err(|e: sqlx::Error| AppError::Internal(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Ok(data
            .render_tpl(
                "reset-password",
                &json!({"error": "Invalid or expired reset token", "token": form.token}),
            )
            .await);
    }

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/login"))
        .finish())
}
