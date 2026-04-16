use crate::{
    AppData, AppError, AppResult, AppRole, Deserialize, HttpResponse, actix_web::get,
    actix_web::http::header::LOCATION, error, hash_password, send_mail, serde_json::json, web,
};

#[derive(Deserialize, Debug)]
pub struct FormData {
    pub email: String,
    pub password: String,
    pub repeat_password: String,
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

pub async fn post(data: web::Data<AppData>, req: actix_web::HttpRequest, form: web::Form<FormData>) -> AppResult {
    use super::RenderTplExt;
    if form.password.len() < 8 {
        return Ok(req
            .render_tpl(
                "register",
                &json!({"error": "Password must be at least 8 characters long"}),
            )
            .await);
    }

    if form.password != form.repeat_password {
        return Ok(req
            .render_tpl("register", &json!({"error": "Passwords do not match"}))
            .await);
    }

    let email = form.email.trim().to_lowercase();
    if !email.contains('@') || email.is_empty() {
        return Ok(req
            .render_tpl("register", &json!({"error": "Invalid email address"}))
            .await);
    }

    let hashed_password =
        hash_password(&form.password).map_err(|e| AppError::Internal(e.to_string()))?;

    let user_exists = sqlx::query!("SELECT id FROM users WHERE email = ?", email)
        .fetch_optional(&data.db)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if user_exists.is_some() {
        if data.email_verification_enabled {
            return Ok(HttpResponse::SeeOther()
                .append_header((LOCATION, "/register-success"))
                .finish());
        }

        return Ok(HttpResponse::SeeOther()
            .append_header((LOCATION, "/login"))
            .finish());
    }

    let is_verified = !data.email_verification_enabled;
    let verification_token = if data.email_verification_enabled {
        Some(uuid::Uuid::new_v4().to_string())
    } else {
        None
    };

    let role = AppRole::User;
    let _user_id = sqlx::query!(
        "INSERT INTO users (email, password, role, is_verified, verification_token) VALUES (?, ?, ?, ?, ?)",
        email,
        hashed_password,
        role,
        is_verified,
        verification_token
    )
    .execute(&data.db)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .last_insert_rowid();

    if data.email_verification_enabled {
        if let Some(token) = verification_token {
            let verify_url = format!("http://{}/verify-email?token={}", data.domain, token);

            let body = match data
                .render_email("emails_verify", &json!({ "verify_url": verify_url }))
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

            actix_web::rt::spawn(async move {
                if let Err(e) = send_mail(&email_clone, "Email Verification", &body) {
                    error!("Failed to send verification email to {email_clone}: {e}");
                }
            });

            return Ok(HttpResponse::SeeOther()
                .append_header((LOCATION, "/register-success"))
                .finish());
        }
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
    let token = match query.get("token") {
        Some(t) => t,
        None => return Ok(HttpResponse::BadRequest().body("Missing token")),
    };

    let result = sqlx::query!(
        "UPDATE users SET is_verified = 1, verification_token = NULL WHERE verification_token = ?",
        token
    )
    .execute(&data.db)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Ok(req
            .render_tpl(
                "login",
                &json!({"error": "Invalid or expired confirmation link"}),
            )
            .await);
    }

    Ok(req
        .render_tpl(
            "login",
            &json!({"success": "Email successfully confirmed. You can now log in."}),
        )
        .await)
}
