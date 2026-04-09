use crate::{
    AppData, AppError, AppResult, Data, DefaultRole, Deserialize, Env, Form, HttpResponse, LOCATION,
    Responder, Role, cookie::Cookie, cookie::time::Duration, create_jwt, get, hash_password, json,
    verify_password,
};
use std::sync::OnceLock;

type AppUser = crate::User<DefaultRole>;

static DUMMY_HASH: OnceLock<String> = OnceLock::new();

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
    let user_res = sqlx::query_as!(
        AppUser,
        "SELECT id, email, password, role as \"role: DefaultRole\", created_at, is_verified, verification_token FROM users WHERE email = $1",
        form.email
    )
    .fetch_one(&data.db)
    .await;

    let (user, user_exists) = match user_res {
        Ok(u) => (Some(u), true),
        Err(sqlx::Error::RowNotFound) => (None, false),
        Err(e) => return Err(e.into()),
    };

    let dummy_hash = DUMMY_HASH.get_or_init(|| {
        hash_password("dummy_password_for_timing_safety").unwrap_or_else(|_| {
            "$argon2id$v=19$m=4096,t=3,p=1$c29tZXNhbHQ$i6PrS9n+AdfNf/U7/lH1XQ".to_string()
        })
    });
    let hash = user.as_ref().map_or(dummy_hash.as_str(), |u| &u.password);

    let password_ok = verify_password(&form.password, hash);

    if !user_exists
        || !password_ok
        || user
            .as_ref()
            .is_none_or(|u| u.role.is_none())
    {
        return Ok(data
            .render_tpl("login", &json!({"error": "Invalid credentials"}))
            .await);
    }

    if let Some(ref u) = user {
        if !u.is_verified {
            return Ok(data
                .render_tpl(
                    "login",
                    &json!({"error": "Please confirm your email address first."}),
                )
                .await);
        }
    }

    let user = match user {
        Some(u) => u,
        None => {
            return Ok(data
                .render_tpl("login", &json!({"error": "Falsche Daten"}))
                .await);
        }
    };

    let jwt = create_jwt(&user, &data.jwt_secret)
        .map_err(|e| AppError::Internal(format!("JWT creation error: {e}")))?;

    let cookie = Cookie::build("token", jwt)
        .domain(&data.domain)
        .path("/")
        .same_site(actix_web::cookie::SameSite::Strict)
        .secure(data.env != Env::Dev)
        .max_age(Duration::hours(1))
        .http_only(true)
        .finish();

    Ok(HttpResponse::SeeOther()
        .append_header((LOCATION, "/"))
        .cookie(cookie)
        .finish())
}
