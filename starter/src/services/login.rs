use crate::{
    AppData, AppError, AppResult, Data, Deserialize, Env, Form, HttpResponse, LOCATION, Responder,
    User, cookie::Cookie, cookie::time::Duration, create_jwt, get, json, verify_password,
};

#[derive(Deserialize)]
pub struct FormData {
    email: String,
    password: String,
}

#[get("/login")]
pub async fn get(data: Data<AppData>) -> impl Responder {
    data.render("login").await
}

pub async fn post(data: Data<AppData>, form: Form<FormData>) -> AppResult {
    let user_res = sqlx::query_as!(User, "SELECT * FROM users WHERE email = $1", form.email)
        .fetch_one(&data.db)
        .await;

    let user = match user_res {
        Ok(u) => u,
        Err(sqlx::Error::RowNotFound) => {
            return Ok(data
                .render_tpl("login", &json!({"error": "Falsche Daten"}))
                .await);
        }
        Err(e) => return Err(e.into()),
    };

    if !verify_password(&form.password, &user.password) {
        return Ok(data
            .render_tpl("login", &json!({"error": "Falsche Daten"}))
            .await);
    }

    if &user.role != "admin" {
        return Ok(data
            .render_tpl("login", &json!({"error": "Falsche Daten"}))
            .await);
    }

    let jwt =
        create_jwt(user).map_err(|e| AppError::Internal(format!("JWT creation error: {}", e)))?;

    let cookie = Cookie::build("token", jwt)
        .domain(&data.domain)
        .path("/")
        .same_site(actix_web::cookie::SameSite::Strict)
        .secure(data.env != Env::Dev)
        .max_age(Duration::hours(12))
        .http_only(true)
        .finish();

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/"))
        .cookie(cookie)
        .finish())
}
