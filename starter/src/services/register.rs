use crate::{
    AppData, AppError, AppResult, Deserialize, HttpResponse, actix_web::get,
    actix_web::http::header::LOCATION, hash_password, serde_json::json, web,
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

    if form.password != form.repeat_password {
        return Ok(data
            .render_tpl(
                "register",
                &json!({"error": "Passwörter stimmen nicht überein"}),
            )
            .await);
    }

    let user_exists = sqlx::query!("SELECT id FROM users WHERE email = ?", form.email)
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

    let hashed_password =
        hash_password(&form.password).map_err(|e| AppError::Internal(e.to_string()))?;

    let _user_id = sqlx::query!(
        "INSERT INTO users (email, password, role) VALUES (?, ?, 'user')",
        form.email,
        hashed_password
    )
    .execute(&data.db)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .last_insert_rowid();

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/login"))
        .finish())
}
