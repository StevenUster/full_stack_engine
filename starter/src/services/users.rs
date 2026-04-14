use crate::{
    AppData, AppResult, AppRole, AuthUser, Deserialize, Serialize, Table, TableHeader,
    actix_web::{HttpResponse, delete, get, post, web},
};

type AppUser = crate::User<AppRole>;

#[derive(Serialize)]
struct Row {
    pub id: i64,
    pub email: String,
    pub role: AppRole,
    pub created_at: String,
    pub link: String,
}

#[get("/users")]
pub async fn get(data: web::Data<AppData>, user: AuthUser<AppRole>) -> AppResult {
    user.require_permission("users.read")?;

    let users = sqlx::query_as!(
        AppUser,
        "SELECT id, email, password, role as \"role: AppRole\", created_at, is_verified, verification_token FROM users ORDER BY created_at DESC"
    )
    .fetch_all(&data.db)
    .await?;

    let rows: Vec<Row> = users
        .into_iter()
        .map(|u| Row {
            id: u.id,
            email: u.email,
            role: u.role,
            created_at: u.created_at.to_string(),
            link: format!("/users/{}", u.id),
        })
        .collect();

    let table = Table {
        headers: vec![
            TableHeader {
                label: "ID".to_string(),
                key: "id".to_string(),
                format: None,
            },
            TableHeader {
                label: "Email".to_string(),
                key: "email".to_string(),
                format: None,
            },
            TableHeader {
                label: "Role".to_string(),
                key: "role".to_string(),
                format: None,
            },
            TableHeader {
                label: "Date".to_string(),
                key: "created_at".to_string(),
                format: None,
            },
            TableHeader {
                label: "Actions".to_string(),
                key: "id".to_string(),
                format: Some("delete_user".to_string()),
            },
        ],
        rows,
        actions: vec![],
    };

    Ok(user
        .render_tpl(
            &data,
            "users",
            &crate::json!({
                "headers": table.headers,
                "rows": table.rows,
                "actions": table.actions,
            }),
        )
        .await)
}

#[get("/users/{id}")]
pub async fn get_user(
    data: web::Data<AppData>,
    user: AuthUser<AppRole>,
    path: web::Path<i64>,
) -> AppResult {
    user.require_permission("users.read")?;

    let user_id = path.into_inner();
    let user_data = sqlx::query_as!(
        AppUser,
        "SELECT id, email, password, role as \"role: AppRole\", created_at, is_verified, verification_token FROM users WHERE id = ?",
        user_id
    )
    .fetch_one(&data.db)
    .await?;

    Ok(user
        .render_tpl(
            &data,
            "user",
            &crate::json!({
                "id": user_data.id,
                "email": user_data.email,
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

    sqlx::query!("UPDATE users SET role = ? WHERE id = ?", form.role, user_id)
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

    sqlx::query!("DELETE FROM users WHERE id = ?", user_id)
        .execute(&data.db)
        .await?;

    Ok(HttpResponse::Ok().finish())
}
