use crate::{
    AppData, AppResult, AppRole, AuthUser, Data, Deserialize, Form, HttpResponse, LOCATION,
    actix_web::get, actix_web::post, error, json, send_mail,
};

#[get("/settings")]
pub async fn get(data: Data<AppData>, req: actix_web::HttpRequest, user: AuthUser<AppRole>) -> AppResult {
    use super::RenderTplExt;
    let user_data = sqlx::query!("SELECT email FROM users WHERE id = ?", user.claims.sub)
        .fetch_one(&data.db)
        .await?;

    Ok(req
        .render_tpl(
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
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    form: Form<ChangeEmailForm>,
) -> AppResult {
    use super::RenderTplExt;
    let new_email = form.new_email.trim().to_lowercase();

    let current = sqlx::query!("SELECT email FROM users WHERE id = ?", user.claims.sub)
        .fetch_one(&data.db)
        .await?;

    if !new_email.contains('@') || new_email.is_empty() {
        return Ok(req
            .render_tpl(
                "settings",
                &json!({"email_error": "Please enter a valid email address.", "current_email": current.email}),
            )
            .await);
    }

    if current.email == new_email {
        return Ok(req
            .render_tpl(
                "settings",
                &json!({"email_error": "This is already your current email.", "current_email": new_email}),
            )
            .await);
    }

    // An attacker could use this to find out if an email exists but I don't see a better option.
    let existing = sqlx::query!("SELECT id FROM users WHERE email = ?", new_email)
        .fetch_optional(&data.db)
        .await?;

    if existing.is_some() {
        return Ok(req
            .render_tpl(
                "settings",
                &json!({"email_error": "This email address is already in use.", "current_email": current.email}),
            )
            .await);
    }

    let token = uuid::Uuid::new_v4().to_string();

    sqlx::query!(
        "UPDATE users SET pending_email = ?, email_change_token = ? WHERE id = ?",
        new_email,
        token,
        user.claims.sub
    )
    .execute(&data.db)
    .await?;

    let verify_url = format!("http://{}/verify-email-change?token={}", data.domain, token);

    let body = match data
        .render_email(
            "emails_verify-email-change",
            &json!({ "verify_url": verify_url }),
        )
        .await
    {
        Ok(html) => html,
        Err(e) => {
            error!("Failed to render email change verification template: {e}");
            return Ok(req
                .render_tpl(
                    "settings",
                    &json!({"email_error": "Failed to send verification email.", "current_email": current.email}),
                )
                .await);
        }
    };

    let email_clone = new_email.clone();
    actix_web::rt::spawn(async move {
        if let Err(e) = send_mail(&email_clone, "Confirm Your New Email", &body) {
            error!("Failed to send email change verification to {email_clone}: {e}");
        }
    });

    Ok(req
        .render_tpl(
            "settings",
            &json!({"email_success": "A verification email has been sent to your new address. Please check your inbox to confirm the change.", "current_email": current.email}),
        )
        .await)
}

#[get("/verify-email-change")]
pub async fn verify_email_change(
    data: Data<AppData>,
    req: actix_web::HttpRequest,
    query: actix_web::web::Query<std::collections::HashMap<String, String>>,
) -> AppResult {
    use super::RenderTplExt;
    let token = match query.get("token") {
        Some(t) => t,
        None => {
            return Ok(req
                .render_tpl("login", &json!({"error": "Missing token"}))
                .await);
        }
    };

    let user_row = sqlx::query!(
        "SELECT id, pending_email FROM users WHERE email_change_token = ?",
        token
    )
    .fetch_optional(&data.db)
    .await?;

    let user_row = match user_row {
        Some(row) => row,
        None => {
            return Ok(req
                .render_tpl(
                    "login",
                    &json!({"error": "Invalid or expired confirmation link."}),
                )
                .await);
        }
    };

    let new_email = match user_row.pending_email {
        Some(email) => email,
        None => {
            return Ok(req
                .render_tpl("login", &json!({"error": "No pending email change found."}))
                .await);
        }
    };

    sqlx::query!(
        "UPDATE users SET email = ?, pending_email = NULL, email_change_token = NULL WHERE id = ?",
        new_email,
        user_row.id
    )
    .execute(&data.db)
    .await?;

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/logout"))
        .finish())
}

#[post("/settings/password-reset")]
pub async fn post_password_reset(data: Data<AppData>, req: actix_web::HttpRequest, user: AuthUser<AppRole>) -> AppResult {
    use super::RenderTplExt;
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
            return Ok(req
                .render_tpl(
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

    Ok(req
        .render_tpl(
            "settings",
            &json!({"success": "Password reset email sent.", "current_email": user_data.email}),
        )
        .await)
}
