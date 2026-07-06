use crate::{
    AppData, AppError, AppResult, AppRole, AuthUser, Deserialize, Serialize,
    actix_web::{HttpResponse, delete, get, post, web},
};
use full_stack_engine::prelude::Role;

type AppUser = crate::User<AppRole>;

const PER_PAGE: i64 = 20;

#[derive(Deserialize, Default)]
pub struct UserSearchParams {
    pub search: Option<String>,
    pub filter_role: Option<String>,
    pub page: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct UserListRecord {
    id: i64,
    email: String,
    role: String,
    created_at: String,
}

#[derive(Serialize)]
struct Row {
    pub id: i64,
    pub email: String,
    pub role: String,
    pub created_at: String,
    pub link: String,
    pub delete_url: String,
}

#[get("/users")]
pub async fn get(
    data: web::Data<AppData>,
    req: actix_web::HttpRequest,
    user: AuthUser<AppRole>,
    query: web::Query<UserSearchParams>,
) -> AppResult {
    use super::RenderTplExt;
    user.require_permission("users.read")?;

    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * PER_PAGE;
    let search = query.search.as_deref().unwrap_or("").trim().to_string();
    let filter_role = query
        .filter_role
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let pattern = format!("%{search}%");

    let (total_count, users) = match (search.is_empty(), filter_role.is_empty()) {
        (true, true) => {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
                .fetch_one(&data.db)
                .await?;
            let rows = sqlx::query_as::<_, UserListRecord>(
                "SELECT id, email, role, created_at FROM users \
                 ORDER BY created_at DESC LIMIT ? OFFSET ?",
            )
            .bind(PER_PAGE)
            .bind(offset)
            .fetch_all(&data.db)
            .await?;
            (count, rows)
        }
        (false, true) => {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE email LIKE ?")
                .bind(&pattern)
                .fetch_one(&data.db)
                .await?;
            let rows = sqlx::query_as::<_, UserListRecord>(
                "SELECT id, email, role, created_at FROM users \
                 WHERE email LIKE ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
            )
            .bind(&pattern)
            .bind(PER_PAGE)
            .bind(offset)
            .fetch_all(&data.db)
            .await?;
            (count, rows)
        }
        (true, false) => {
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE role = ?")
                .bind(&filter_role)
                .fetch_one(&data.db)
                .await?;
            let rows = sqlx::query_as::<_, UserListRecord>(
                "SELECT id, email, role, created_at FROM users \
                 WHERE role = ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
            )
            .bind(&filter_role)
            .bind(PER_PAGE)
            .bind(offset)
            .fetch_all(&data.db)
            .await?;
            (count, rows)
        }
        (false, false) => {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE email LIKE ? AND role = ?")
                    .bind(&pattern)
                    .bind(&filter_role)
                    .fetch_one(&data.db)
                    .await?;
            let rows = sqlx::query_as::<_, UserListRecord>(
                "SELECT id, email, role, created_at FROM users \
                 WHERE email LIKE ? AND role = ? ORDER BY created_at DESC LIMIT ? OFFSET ?",
            )
            .bind(&pattern)
            .bind(&filter_role)
            .bind(PER_PAGE)
            .bind(offset)
            .fetch_all(&data.db)
            .await?;
            (count, rows)
        }
    };

    let total_pages = ((total_count + PER_PAGE - 1) / PER_PAGE).max(1);

    let rows: Vec<Row> = users
        .into_iter()
        .map(|u| Row {
            id: u.id,
            email: u.email,
            role: u.role,
            created_at: u.created_at,
            link: format!("/users/{}", u.id),
            delete_url: format!("/users/{}", u.id),
        })
        .collect();

    Ok(req
        .render_tpl(
            "users",
            &crate::json!({
                "rows": rows,
                "search": search,
                "filter_role": filter_role,
                "roles": AppRole::all_roles(),
                "page": page,
                "total_pages": total_pages,
                "total_count": total_count,
                "per_page": PER_PAGE,
            }),
        )
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
    .fetch_one(&data.db)
    .await?;

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

#[post("/users/{id}")]
pub async fn post_user(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
    form: web::Form<UserUpdateForm>,
) -> AppResult {
    user.require_permission("users.write")?;

    let user_id = path.into_inner();

    // Only accept input that names a real role, and store the canonical
    // value; a typo would otherwise silently map to the no-access role.
    let new_role = AppRole::from_role_str(&form.role);
    if new_role.as_str() != form.role.trim().to_lowercase() {
        return Err(AppError::User("Unknown role".to_string()));
    }

    // Defense in depth: only admins may touch admin accounts or hand out an
    // admin role, so a future role holding `users.write` can never be used to
    // promote itself (or a colluding account) past its own privileges.
    if (new_role.is_admin() || target_role(&data, user_id).await?.is_admin())
        && !user.claims.role.is_admin()
    {
        return Err(AppError::NoAuth);
    }

    // Role change invalidates the target user's existing sessions.
    let now = super::now_unix();
    sqlx::query!(
        "UPDATE users SET role = ?, sessions_valid_after = ? WHERE id = ?",
        new_role,
        now,
        user_id
    )
    .execute(&data.db)
    .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", "/users"))
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

/// Loads the target user's role, for the admin guards above.
async fn target_role(data: &web::Data<AppData>, user_id: i64) -> Result<AppRole, AppError> {
    let row = sqlx::query!("SELECT role FROM users WHERE id = ?", user_id)
        .fetch_optional(&data.db)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    Ok(AppRole::from_role_str(&row.role))
}
