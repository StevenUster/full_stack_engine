use crate::{
    AppData, AppResult, AppRole, AuthUser, Data, Deserialize, Form, HttpResponse, LOCATION,
    actix_web::get, actix_web::post, error, json, send_mail,
};

pub(crate) async fn settings_context(
    data: &AppData,
    user_id: i64,
    overrides: crate::serde_json::Value,
) -> Result<crate::serde_json::Value, sqlx::Error> {
    let user_data = sqlx::query!(
        "SELECT email, first_name, last_name FROM users WHERE id = ?",
        user_id
    )
    .fetch_one(&data.db)
    .await?;

    let mut ctx = json!({
        "current_email": user_data.email,
        "first_name": user_data.first_name.unwrap_or_default(),
        "last_name": user_data.last_name.unwrap_or_default(),
    });

    if let (Some(obj), Some(over)) = (ctx.as_object_mut(), overrides.as_object()) {
        for (k, v) in over {
            obj.insert(k.clone(), v.clone());
        }
    }

    Ok(ctx)
}

#[get("/settings")]
pub async fn get(
    data: Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
) -> AppResult {
    use super::RenderTplExt;
    let ctx = settings_context(&data, user.claims.sub, json!({})).await?;
    Ok(req.render_tpl("settings", &ctx).await)
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
        let ctx = settings_context(
            &data,
            user.claims.sub,
            json!({"email_error": "invalid_email"}),
        )
        .await?;
        return Ok(req.render_tpl("settings", &ctx).await);
    }

    if current.email == new_email {
        let ctx = settings_context(
            &data,
            user.claims.sub,
            json!({"email_error": "already_current_email"}),
        )
        .await?;
        return Ok(req.render_tpl("settings", &ctx).await);
    }

    // An attacker could use this to find out if an email exists but I don't see a better option.
    let existing = sqlx::query!("SELECT id FROM users WHERE email = ?", new_email)
        .fetch_optional(&data.db)
        .await?;

    if existing.is_some() {
        let ctx = settings_context(
            &data,
            user.claims.sub,
            json!({"email_error": "email_in_use"}),
        )
        .await?;
        return Ok(req.render_tpl("settings", &ctx).await);
    }

    if !data.email_verification_enabled {
        sqlx::query!(
            "UPDATE users SET email = ?, pending_email = NULL, email_change_token = NULL WHERE id = ?",
            new_email,
            user.claims.sub
        )
        .execute(&data.db)
        .await?;

        let ctx = settings_context(
            &data,
            user.claims.sub,
            json!({"email_success": "email_updated"}),
        )
        .await?;
        return Ok(req.render_tpl("settings", &ctx).await);
    }

    let token = uuid::Uuid::new_v4().to_string();
    let expires_at = super::token_expiry();

    sqlx::query!(
        "UPDATE users SET pending_email = ?, email_change_token = ?, email_change_token_expires_at = ? WHERE id = ?",
        new_email,
        token,
        expires_at,
        user.claims.sub
    )
    .execute(&data.db)
    .await?;

    let verify_url = format!(
        "{}://{}/verify-email-change?token={}",
        data.protocol, data.domain, token
    );

    let t = super::load_locale("en");
    let body = match data
        .render_email(
            "emails_verify-email-change",
            &json!({
                "t": t,
                "verify_url": verify_url,
                "base_url": format!("{}://{}", data.protocol, data.domain),
            }),
        )
        .await
    {
        Ok(html) => html,
        Err(e) => {
            error!("Failed to render email change verification template: {e}");
            let ctx = settings_context(
                &data,
                user.claims.sub,
                json!({"email_error": "send_email_failed"}),
            )
            .await?;
            return Ok(req.render_tpl("settings", &ctx).await);
        }
    };

    let email_clone = new_email.clone();
    let subject = t["verify_email_change"]["subject"]
        .as_str()
        .unwrap_or("Confirm Your New Email")
        .to_string();
    actix_web::rt::spawn(async move {
        if let Err(e) = send_mail(&email_clone, &subject, &body).await {
            error!("Failed to send email change verification to {email_clone}: {e}");
        }
    });

    let ctx = settings_context(
        &data,
        user.claims.sub,
        json!({"email_success": "email_verification_sent"}),
    )
    .await?;
    Ok(req.render_tpl("settings", &ctx).await)
}

#[get("/verify-email-change")]
pub async fn verify_email_change(
    data: Data<AppData>,
    req: actix_web::HttpRequest,
    query: actix_web::web::Query<std::collections::HashMap<String, String>>,
) -> AppResult {
    use super::RenderTplExt;
    let Some(token) = query.get("token") else {
        return Ok(req
            .render_tpl("login", &json!({"error": "missing_token"}))
            .await);
    };

    let user_row = sqlx::query!(
        "SELECT id, pending_email FROM users WHERE email_change_token = ? \
         AND email_change_token_expires_at IS NOT NULL AND email_change_token_expires_at > CURRENT_TIMESTAMP",
        token
    )
    .fetch_optional(&data.db)
    .await?;

    let Some(user_row) = user_row else {
        return Ok(req
            .render_tpl("login", &json!({"error": "invalid_token"}))
            .await);
    };

    let Some(new_email) = user_row.pending_email else {
        return Ok(req
            .render_tpl("login", &json!({"error": "no_pending_change"}))
            .await);
    };

    // Changing the account email is an identity change: bump
    // `sessions_valid_after` so all previously issued JWTs are rejected, not
    // just the cookie of the browser that clicked the link.
    let now = super::now_unix();
    sqlx::query!(
        "UPDATE users SET email = ?, pending_email = NULL, email_change_token = NULL, email_change_token_expires_at = NULL, sessions_valid_after = ? WHERE id = ?",
        new_email,
        now,
        user_row.id
    )
    .execute(&data.db)
    .await?;

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/logout"))
        .finish())
}

#[post("/settings/password-reset")]
pub async fn post_password_reset(
    data: Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
) -> AppResult {
    use super::RenderTplExt;
    let token = uuid::Uuid::new_v4().to_string();
    let expires_at = super::token_expiry();

    let user_data = sqlx::query!("SELECT email FROM users WHERE id = ?", user.claims.sub)
        .fetch_one(&data.db)
        .await?;

    sqlx::query!(
        "UPDATE users SET reset_token = ?, reset_token_expires_at = ? WHERE id = ?",
        token,
        expires_at,
        user.claims.sub
    )
    .execute(&data.db)
    .await?;

    let reset_url = format!(
        "{}://{}/reset-password?token={}",
        data.protocol, data.domain, token
    );

    let t = super::load_locale("en");
    let body = match data
        .render_email(
            "emails_password-reset",
            &json!({
                "t": t,
                "reset_url": reset_url,
                "base_url": format!("{}://{}", data.protocol, data.domain),
            }),
        )
        .await
    {
        Ok(html) => html,
        Err(e) => {
            error!("Failed to render password reset email template: {e}");
            let ctx = settings_context(
                &data,
                user.claims.sub,
                json!({"error": "send_email_failed"}),
            )
            .await?;
            return Ok(req.render_tpl("settings", &ctx).await);
        }
    };

    let email = user_data.email.clone();
    let subject = t["password_reset_email"]["subject"]
        .as_str()
        .unwrap_or("Password Reset")
        .to_string();
    actix_web::rt::spawn(async move {
        if let Err(e) = send_mail(&email, &subject, &body).await {
            error!("Failed to send password reset email to {email}: {e}");
        }
    });

    let ctx = settings_context(
        &data,
        user.claims.sub,
        json!({"success": "password_reset_sent"}),
    )
    .await?;
    Ok(req.render_tpl("settings", &ctx).await)
}

/// Permanently deletes the caller's own account. Apps with per-user assets
/// (uploaded files, etc.) should clean those up here before the `DELETE`.
#[post("/settings/delete-account")]
pub async fn post_delete_account(data: Data<AppData>, user: AuthUser<AppRole>) -> AppResult {
    sqlx::query!("DELETE FROM users WHERE id = ?", user.claims.sub)
        .execute(&data.db)
        .await?;

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/logout"))
        .finish())
}
