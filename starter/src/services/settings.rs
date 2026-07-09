use crate::{
    AppData, AppResult, AppRole, AuthUser, Data, Deserialize, Form, HttpResponse, LOCATION,
    actix_web::get, actix_web::post, error, find_one, json, send_mail, update,
};

use crate::chrono::NaiveDateTime;
use crate::tables::user::User;

pub(crate) async fn settings_context(
    data: &AppData,
    user_id: i64,
    overrides: crate::serde_json::Value,
) -> Result<crate::serde_json::Value, sqlx::Error> {
    let user_data = User::fetch(&data.db, user_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;

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

// Registered in `services::configure` behind a strict rate limiter (this
// sends an email to a caller-chosen address), not via a route attribute.
pub async fn post_change_email(
    data: Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    form: Form<ChangeEmailForm>,
) -> AppResult {
    use super::RenderTplExt;
    let new_email = form.new_email.trim().to_lowercase();

    let current = User::fetch(&data.db, user.claims.sub)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;

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
    if User::fetch_by_email(&data.db, &new_email).await?.is_some() {
        let ctx = settings_context(
            &data,
            user.claims.sub,
            json!({"email_error": "email_in_use"}),
        )
        .await?;
        return Ok(req.render_tpl("settings", &ctx).await);
    }

    if !data.email_verification_enabled {
        update!(
            User,
            &data.db,
            id == user.claims.sub;
            email = new_email.clone(),
            pending_email = None::<String>,
            email_change_token = None::<String>
        )
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

    update!(
        User,
        &data.db,
        id == user.claims.sub;
        pending_email = Some(new_email.clone()),
        email_change_token = Some(token.clone()),
        email_change_token_expires_at = Some(expires_at)
    )
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

    // An expired token matches no row (`NULL > x` is never true).
    let user_row = find_one!(
        User,
        &data.db,
        email_change_token == token.as_str() && email_change_token_expires_at > super::now()
    )
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
    update!(
        User,
        &data.db,
        id == user_row.id;
        email = new_email,
        pending_email = None::<String>,
        email_change_token = None::<String>,
        email_change_token_expires_at = None::<NaiveDateTime>,
        sessions_valid_after = now
    )
    .await?;

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/logout"))
        .finish())
}

// Registered in `services::configure` behind a strict rate limiter (this
// sends an email), not via a route attribute.
pub async fn post_password_reset(
    data: Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
) -> AppResult {
    use super::RenderTplExt;
    let token = uuid::Uuid::new_v4().to_string();
    let expires_at = super::token_expiry();

    let user_data = User::fetch(&data.db, user.claims.sub)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;

    update!(
        User,
        &data.db,
        id == user.claims.sub;
        reset_token = Some(token.clone()),
        reset_token_expires_at = Some(expires_at)
    )
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
/// (uploaded files, etc.) should clean those up here before the delete.
#[post("/settings/delete-account")]
pub async fn post_delete_account(data: Data<AppData>, user: AuthUser<AppRole>) -> AppResult {
    User::delete(&data.db, user.claims.sub).await?;

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/logout"))
        .finish())
}
