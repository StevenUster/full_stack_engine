use crate::{
    AppData, AppError, AppResult, Deserialize, HttpResponse, actix_web::get,
    actix_web::http::header::LOCATION, error, hash_password, send_mail, serde_json::json, web,
};

#[derive(Deserialize, Debug)]
pub struct FormData {
    pub email: String,
    pub password: String,
    pub repeat_password: String,
    // you can add a register key here to only allow trusted registers
    // pub register_key: String,
}

#[get("/register")]
pub async fn get(data: web::Data<AppData>) -> HttpResponse {
    data.render("register").await
}

#[get("/register-success")]
pub async fn register_success(data: web::Data<AppData>) -> HttpResponse {
    data.render("register-success").await
}

pub async fn post(data: web::Data<AppData>, form: web::Form<FormData>) -> AppResult {
    // Optional
    // if let Some(register_key) = &data.register_key {
    //     if form.register_key != *register_key {
    //         return Ok(data
    //             .render_tpl("register", &json!({"error": "Falscher Register-Schlüssel"}))
    //             .await);
    //     }
    // }

    if form.password.len() < 8 {
        return Ok(data
            .render_tpl(
                "register",
                &json!({"error": "Password must be at least 8 characters long"}),
            )
            .await);
    }

    if form.password != form.repeat_password {
        return Ok(data
            .render_tpl(
                "register",
                &json!({"error": "Passwords do not match"}),
            )
            .await);
    }

    let email = form.email.trim().to_lowercase();
    if !email.contains('@') || email.is_empty() {
        return Ok(data
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

    let _user_id = sqlx::query!(
        "INSERT INTO users (email, password, role, is_verified, verification_token) VALUES (?, ?, ?, ?, ?)",
        email,
        hashed_password,
        crate::UserRole::User,
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
                    error!("Failed to render verification email template: {}", e);
                    return Err(AppError::Internal(
                        "Failed to render email template".to_string(),
                    ));
                }
            };

            let email_clone = email.clone();

            // Sending email in a separate task to not block registration response
            actix_web::rt::spawn(async move {
                if let Err(e) = send_mail(&email_clone, "Email Verification", &body) {
                    error!(
                        "Failed to send verification email to {}: {}",
                        email_clone, e
                    );
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
    query: web::Query<std::collections::HashMap<String, String>>,
) -> AppResult {
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
        return Ok(data
            .render_tpl(
                "login",
                &json!({"error": "Invalid or expired confirmation link"}),
            )
            .await);
    }

    Ok(data
        .render_tpl(
            "login",
            &json!({"success": "Email successfully confirmed. You can now log in."}),
        )
        .await)
}
