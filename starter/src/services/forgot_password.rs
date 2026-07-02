use crate::{AppData, AppResult, Data, Deserialize, Form, actix_web::get, error, json, send_mail};

#[get("/forgot-password")]
pub async fn get(req: actix_web::HttpRequest) -> impl actix_web::Responder {
    use super::RenderTplExt;
    req.render_tpl("forgot-password", &json!({})).await
}

#[derive(Deserialize)]
pub struct ForgotPasswordForm {
    email: String,
}

pub async fn post(
    data: Data<AppData>,
    req: actix_web::HttpRequest,
    form: Form<ForgotPasswordForm>,
) -> AppResult {
    use super::RenderTplExt;

    let user = sqlx::query!("SELECT id, email FROM users WHERE email = ?", form.email)
        .fetch_optional(&data.db)
        .await?;

    // Always show the same success message, whether or not the email exists,
    // so this endpoint can't be used to enumerate registered accounts.
    let success_ctx = json!({"success": "password_reset_sent"});

    let Some(user) = user else {
        return Ok(req.render_tpl("forgot-password", &success_ctx).await);
    };

    let token = uuid::Uuid::new_v4().to_string();

    sqlx::query!(
        "UPDATE users SET reset_token = ? WHERE id = ?",
        token,
        user.id
    )
    .execute(&data.db)
    .await?;

    let reset_url = format!(
        "{}://{}/reset-password?token={}",
        data.protocol, data.domain, token
    );

    let t = super::load_locale("en");
    let body = match data
        .render_email("emails_password-reset", &json!({ "t": t, "reset_url": reset_url }))
        .await
    {
        Ok(html) => html,
        Err(e) => {
            error!("Failed to render password reset email template: {e}");
            return Ok(req
                .render_tpl("forgot-password", &json!({"error": "send_email_failed"}))
                .await);
        }
    };

    let email = user.email.clone();
    let subject = t["password_reset_email"]["subject"]
        .as_str()
        .unwrap_or("Password Reset")
        .to_string();
    actix_web::rt::spawn(async move {
        if let Err(e) = send_mail(&email, &subject, &body).await {
            error!("Failed to send password reset email to {email}: {e}");
        }
    });

    Ok(req.render_tpl("forgot-password", &success_ctx).await)
}
