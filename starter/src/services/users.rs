use crate::{
    AppData, AppError, AppResult, AppRole, AuthUser, Deserialize, Serialize,
    actix_web::{HttpResponse, delete, get, post, web},
};
use full_stack_engine::prelude::Role;

type AppUser = crate::User<AppRole>;

#[derive(Serialize)]
struct Row {
    pub id: i64,
    pub email: String,
    pub role: AppRole,
    pub created_at: String,
    pub link: String,
    pub delete_url: String,
}

#[get("/users")]
pub async fn get(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("users.read")?;

    let users = sqlx::query_as!(
        AppUser,
        "SELECT id, email, password, role as \"role: AppRole\", created_at, is_verified, verification_token FROM users ORDER BY created_at DESC"
    )
    .fetch_all(&data.db)
    .await?;

    // Column config (headers, formats) is page-static and lives in the
    // frontend (`users.astro`); the service only supplies the data.
    let rows: Vec<Row> = users
        .into_iter()
        .map(|u| Row {
            id: u.id,
            email: u.email,
            role: u.role,
            created_at: u.created_at.to_string(),
            link: format!("/users/{}", u.id),
            delete_url: format!("/users/{}", u.id),
        })
        .collect();

    Ok(req
        .render_tpl("users", &crate::json!({ "rows": rows }))
        .await)
}

#[get("/users/{id}")]
pub async fn get_user(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("users.read")?;

    let user_id = path.into_inner();
    let user_data = sqlx::query_as!(
        AppUser,
        "SELECT id, email, password, role as \"role: AppRole\", created_at, is_verified, verification_token FROM users WHERE id = ?",
        user_id
    )
    .fetch_optional(&data.db)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    Ok(req
        .render_tpl(
            "user",
            &crate::json!({
                "id": user_data.id,
                "email": user_data.email,
                "role": user_data.role.as_str(),
                "roles": AppRole::all_roles(),
            }),
        )
        .await)
}

#[derive(Deserialize)]
pub struct UserUpdateForm {
    pub role: String,
}

/// Loads the target user's role, for the admin guards below.
async fn target_role(data: &web::Data<AppData>, user_id: i64) -> Result<AppRole, AppError> {
    let row = sqlx::query!("SELECT role FROM users WHERE id = ?", user_id)
        .fetch_optional(&data.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    Ok(AppRole::from_role_str(&row.role))
}

#[post("/users/{id}")]
pub async fn post_user(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
    form: web::Form<UserUpdateForm>,
) -> AppResult {
    user.require_permission("users.write")?;

    let user_id = path.into_inner();

    // `from_role_str` maps unknown strings to the no-access role; only accept
    // input that names a real role, and store the canonical value instead of
    // the raw form string.
    let new_role = AppRole::from_role_str(&form.role);
    if new_role.as_str() != form.role.trim().to_lowercase() {
        return Err(AppError::BadRequest("Unknown role".to_string()));
    }

    // Only admins may touch admin accounts or hand out the admin role —
    // otherwise anyone with `users.write` could promote themselves (or a
    // colluding account) to admin, or demote an admin.
    if (new_role.is_admin() || target_role(&data, user_id).await?.is_admin())
        && !user.claims.role.is_admin()
    {
        return Err(AppError::NoAuth);
    }

    // One statement, so the role change and the session invalidation are
    // atomic: the framework's AuthUser extractor rejects any JWT issued
    // before `sessions_valid_after`.
    sqlx::query!(
        "UPDATE users SET role = ?, sessions_valid_after = strftime('%s','now') WHERE id = ?",
        new_role,
        user_id
    )
    .execute(&data.db)
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", format!("/users/{user_id}")))
        .finish())
}

#[delete("/users/{id}")]
pub async fn delete_user(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
) -> AppResult {
    user.require_permission("users.write")?;

    let user_id = path.into_inner();

    // Same guard as `post_user`: only admins may remove admin accounts.
    if target_role(&data, user_id).await?.is_admin() && !user.claims.role.is_admin() {
        return Err(AppError::NoAuth);
    }

    sqlx::query!("DELETE FROM users WHERE id = ?", user_id)
        .execute(&data.db)
        .await?;

    Ok(HttpResponse::Ok().finish())
}
