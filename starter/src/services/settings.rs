use crate::{
    AppData, AppResult, AppRole, AuthUser, Data, Responder, actix_web::get, actix_web::post, error,
    json, send_mail,
};

#[get("/settings")]
pub async fn get(data: Data<AppData>, user: AuthUser<AppRole>) -> impl Responder {
    user.render_tpl(&data, "settings", &json!({})).await
}

#[post("/settings/password-reset")]
pub async fn post_password_reset(data: Data<AppData>, user: AuthUser<AppRole>) -> AppResult {
    let token = uuid::Uuid::new_v4().to_string();

    let user_data = sqlx::query!("SELECT email FROM users WHERE id = ?", user.claims.sub)
        .fetch_one(&data.db)
        .await?;

    sqlx::query!(
        "UPDATE users SET reset_token = ? WHERE id = ?",
        token,
        user.claims.sub
    )
    .execute(&data.db)
    .await?;

    let reset_url = format!("http://{}/reset-password?token={}", data.domain, token);

    let body = match data
        .render_email("emails_password-reset", &json!({ "reset_url": reset_url }))
        .await
    {
        Ok(html) => html,
        Err(e) => {
            error!("Failed to render password reset email template: {e}");
            return Ok(user
                .render_tpl(
                    &data,
                    "settings",
                    &json!({"error": "Failed to generate password reset email"}),
                )
                .await);
        }
    };

    let email = user_data.email.clone();
    actix_web::rt::spawn(async move {
        if let Err(e) = send_mail(&email, "Password Reset", &body) {
            error!("Failed to send password reset email to {email}: {e}");
        }
    });

    Ok(user
        .render_tpl(
            &data,
            "settings",
            &json!({"success": "Password reset email sent."}),
        )
        .await)
}
