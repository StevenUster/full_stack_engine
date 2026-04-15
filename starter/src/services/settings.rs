use crate::{
    AppData, AppResult, AppRole, AuthUser, Data, Deserialize, Form, actix_web::get,
    actix_web::post, error, json, send_mail,
};

#[get("/settings")]
pub async fn get(data: Data<AppData>, user: AuthUser<AppRole>) -> AppResult {
    let user_data = sqlx::query!("SELECT email FROM users WHERE id = ?", user.claims.sub)
        .fetch_one(&data.db)
        .await?;

    Ok(user
        .render_tpl(
            &data,
            "settings",
            &json!({ "current_email": user_data.email }),
        )
        .await)
}

#[derive(Deserialize)]
pub struct ChangeEmailForm {
    pub new_email: String,
}

#[post("/settings/change-email")]
pub async fn post_change_email(
    data: Data<AppData>,
    user: AuthUser<AppRole>,
    form: Form<ChangeEmailForm>,
) -> AppResult {
    let new_email = form.new_email.trim().to_lowercase();

    if !new_email.contains('@') || new_email.is_empty() {
        return Ok(user
            .render_tpl(
                &data,
                "settings",
                &json!({"email_error": "Please enter a valid email address.", "current_email": new_email}),
            )
            .await);
    }

    let current = sqlx::query!("SELECT email FROM users WHERE id = ?", user.claims.sub)
        .fetch_one(&data.db)
        .await?;

    if current.email == new_email {
        return Ok(user
            .render_tpl(
                &data,
                "settings",
                &json!({"email_error": "This is already your current email.", "current_email": new_email}),
            )
            .await);
    }

    // A attacker could use this to find out if an email exists but I don't see a better option.
    let existing = sqlx::query!("SELECT id FROM users WHERE email = ?", new_email)
        .fetch_optional(&data.db)
        .await?;

    if existing.is_some() {
        return Ok(user
            .render_tpl(
                &data,
                "settings",
                &json!({"email_error": "This email address is already in use.", "current_email": current.email}),
            )
            .await);
    }

    sqlx::query!(
        "UPDATE users SET email = ? WHERE id = ?",
        new_email,
        user.claims.sub
    )
    .execute(&data.db)
    .await?;

    Ok(user
        .render_tpl(
            &data,
            "settings",
            &json!({"email_success": "Email address updated successfully.", "current_email": new_email}),
        )
        .await)
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
                    &json!({"error": "Failed to generate password reset email", "current_email": user_data.email}),
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
            &json!({"success": "Password reset email sent.", "current_email": user_data.email}),
        )
        .await)
}
