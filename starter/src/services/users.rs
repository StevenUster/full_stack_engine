use crate::{
    AppData, AppError, AppResult, AppRole, AuthUser, Deserialize, Serialize,
    actix_web::{HttpResponse, delete, get, post, web},
    find_page, update,
};
use full_stack_engine::prelude::Role;

use crate::tables::user::User;

const PER_PAGE: i64 = 20;

#[derive(Deserialize, Default)]
pub struct UserSearchParams {
    pub search: Option<String>,
    pub filter_role: Option<String>,
    pub page: Option<i64>,
}

#[derive(Serialize)]
struct Row {
    pub id: i64,
    pub email: String,
    pub role: &'static str,
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
    let search = query.search.as_deref().unwrap_or("").trim().to_string();
    let filter_role = query
        .filter_role
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    // `eq_opt`: None means "no role filter"; an unknown role string simply
    // matches no rows (same as before).
    let role_filter = (!filter_role.is_empty()).then_some(filter_role.as_str());

    let result = find_page!(
        User,
        &data.db,
        email.contains_opt(&search) && role.eq_opt(role_filter),
        order_by: created_at.desc(),
        page: page,
        per_page: PER_PAGE
    )
    .await?;

    let total_pages = ((result.total + PER_PAGE - 1) / PER_PAGE).max(1);

    let rows: Vec<Row> = result
        .rows
        .into_iter()
        .map(|u| Row {
            id: u.id,
            email: u.email,
            role: u.role.as_str(),
            created_at: u.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
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
                "total_count": result.total,
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

    let user_data = User::fetch(&data.db, path.into_inner())
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
    update!(
        User,
        &data.db,
        id == user_id;
        role = new_role,
        sessions_valid_after = now
    )
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

    User::delete(&data.db, user_id).await?;

    Ok(HttpResponse::Ok().finish())
}

/// Loads the target user's role, for the admin guards above.
async fn target_role(data: &web::Data<AppData>, user_id: i64) -> Result<AppRole, AppError> {
    let target = User::fetch(&data.db, user_id)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    Ok(target.role)
}
