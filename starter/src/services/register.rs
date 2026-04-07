use crate::{
    AppData, AppError, AppResult, Deserialize, HttpResponse, actix_web::get,
    actix_web::http::header::LOCATION, hash_password, send_mail, serde_json::json, web,
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
                &json!({"error": "Passwort muss mindestens 8 Zeichen lang sein"}),
            )
            .await);
    }

    if form.password != form.repeat_password {
        return Ok(data
            .render_tpl(
                "register",
                &json!({"error": "Passwörter stimmen nicht überein"}),
            )
            .await);
    }

    let email = form.email.trim().to_lowercase();
    if !email.contains('@') || email.is_empty() {
        return Ok(data
            .render_tpl("register", &json!({"error": "Ungültige E-Mail-Adresse"}))
            .await);
    }

    let hashed_password =
        hash_password(&form.password).map_err(|e| AppError::Internal(e.to_string()))?;

    let user_exists = sqlx::query!("SELECT id FROM users WHERE email = ?", email)
        .fetch_optional(&data.db)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if user_exists.is_some() {
        return Ok(data
            .render_tpl(
                "register",
                &json!({"error": "E-Mail wird bereits verwendet"}),
            )
            .await);
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
            let body = format!(
                "<h1>Herzlich Willkommen!</h1><p>Bitte bestätigen Sie Ihre E-Mail-Adresse, indem Sie auf den folgenden Link klicken:</p><p><a href=\"{}\">E-Mail bestätigen</a></p>",
                verify_url
            );
            
            let email_clone = email.clone();
            let smtp_from = data.smtp_from.clone();
            
            // Sending email in a separate task to not block registration response
            tokio::spawn(async move {
                if let Err(e) = send_mail(&email_clone, "E-Mail Bestätigung", &body) {
                    log::error!("Failed to send verification email to {}: {}", email_clone, e);
                }
            });

            return Ok(data
                .render_tpl(
                    "register",
                    &json!({"success": "Ein Bestätigungslink wurde an Ihre E-Mail-Adresse gesendet."}),
                )
                .await);
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
            .render_tpl("login", &json!({"error": "Ungültiger oder abgelaufener Bestätigungslink"}))
            .await);
    }

    Ok(data
        .render_tpl("login", &json!({"success": "E-Mail erfolgreich bestätigt. Sie können sich nun einloggen."}))
        .await)
}
