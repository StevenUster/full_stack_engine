use crate::{
    AppData, AppError, AppResult, AppRole, HttpResponse, actix_web::get,
    actix_web::http::header::LOCATION, error, hash_password, send_mail, serde_json::json, update,
    web,
};
use actix_multipart::form::{MultipartForm, text::Text};

use crate::tables::user::{InsertUser, User};

#[derive(MultipartForm)]
pub struct RegisterForm {
    first_name: Text<String>,
    last_name: Text<String>,
    email: Text<String>,
    password: Text<String>,
    repeat_password: Text<String>,
}

#[get("/register")]
pub async fn get(req: actix_web::HttpRequest) -> actix_web::HttpResponse {
    use super::RenderTplExt;
    req.render_tpl("register", &json!({})).await
}

#[get("/register-success")]
pub async fn register_success(req: actix_web::HttpRequest) -> actix_web::HttpResponse {
    use super::RenderTplExt;
    req.render_tpl("register-success", &json!({})).await
}

pub async fn post(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    MultipartForm(form): MultipartForm<RegisterForm>,
) -> AppResult {
    use super::RenderTplExt;

    let first_name = form.first_name.0.trim().to_string();
    let last_name = form.last_name.0.trim().to_string();
    let email = form.email.0.trim().to_lowercase();

    // Values echoed back into the form so the user does not have to retype them on error.
    let form_values = json!({
        "first_name": first_name,
        "last_name": last_name,
        "email": form.email.0,
    });
    let render_error = |error: &str| {
        let mut ctx = form_values.clone();
        ctx["error"] = json!(error);
        ctx
    };

    if first_name.is_empty() || last_name.is_empty() {
        return Ok(req
            .render_tpl("register", &render_error("missing_name"))
            .await);
    }

    if form.password.0.len() < 8 {
        return Ok(req
            .render_tpl("register", &render_error("password_too_short"))
            .await);
    }

    if form.password.0 != form.repeat_password.0 {
        return Ok(req
            .render_tpl("register", &render_error("passwords_mismatch"))
            .await);
    }

    if !email.contains('@') || email.is_empty() {
        return Ok(req
            .render_tpl("register", &render_error("invalid_email"))
            .await);
    }

    let hashed_password =
        hash_password(&form.password.0).map_err(|e| AppError::Internal(e.to_string()))?;

    let is_verified = !data.email_verification_enabled;
    let verification_token = if data.email_verification_enabled {
        Some(uuid::Uuid::new_v4().to_string())
    } else {
        None
    };
    let verification_token_expires_at = verification_token.as_ref().map(|_| super::token_expiry());

    let insert_result = InsertUser {
        role: AppRole::User,
        is_verified,
        first_name: Some(first_name.clone()),
        last_name: Some(last_name.clone()),
        verification_token: verification_token.clone(),
        verification_token_expires_at,
        ..InsertUser::new(email.clone(), hashed_password)
    }
    .insert(&data.db)
    .await;

    match insert_result {
        Ok(_) => {}
        // The email is already registered. Relying on the UNIQUE constraint
        // (instead of a check-then-insert, which races with a concurrent
        // registration) and responding exactly like a successful registration
        // keeps this endpoint from being used to enumerate accounts.
        Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
            let location = if data.email_verification_enabled {
                "/register-success"
            } else {
                "/login"
            };
            return Ok(HttpResponse::SeeOther()
                .append_header((LOCATION, location))
                .finish());
        }
        Err(e) => return Err(AppError::Internal(e.to_string())),
    }

    if data.email_verification_enabled
        && let Some(token) = verification_token
    {
        let verify_url = format!(
            "{}://{}/verify-email?token={}",
            data.protocol, data.domain, token
        );

        let t = super::load_locale("en");
        let body = match data
            .render_email(
                "emails_verify",
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
                error!("Failed to render verification email template: {e}");
                return Err(AppError::Internal(
                    "Failed to render email template".to_string(),
                ));
            }
        };

        let email_clone = email.clone();
        let subject = t["verify_email"]["subject"]
            .as_str()
            .unwrap_or("Email Verification")
            .to_string();

        actix_web::rt::spawn(async move {
            if let Err(e) = send_mail(&email_clone, &subject, &body).await {
                error!("Failed to send verification email to {email_clone}: {e}");
            }
        });

        return Ok(HttpResponse::SeeOther()
            .append_header((LOCATION, "/register-success"))
            .finish());
    }

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/login"))
        .finish())
}

#[get("/verify-email")]
pub async fn verify_email(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    query: web::Query<std::collections::HashMap<String, String>>,
) -> AppResult {
    use super::RenderTplExt;
    let Some(token) = query.get("token") else {
        return Ok(HttpResponse::BadRequest().body("Missing token"));
    };

    // An expired token matches no row (`NULL > x` is never true, so a
    // missing expiry can't verify either).
    let updated = update!(
        User,
        &data.db,
        verification_token == token.as_str() && verification_token_expires_at > super::now();
        is_verified = true,
        verification_token = None::<String>,
        verification_token_expires_at = None::<crate::chrono::NaiveDateTime>
    )
    .await?;

    if updated == 0 {
        return Ok(req
            .render_tpl("login", &json!({"error": "invalid_token"}))
            .await);
    }

    Ok(req
        .render_tpl("login", &json!({"success": "email_confirmed"}))
        .await)
}
