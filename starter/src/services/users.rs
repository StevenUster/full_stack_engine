use crate::{
    actix_web::{delete, get, post, web, HttpResponse},
    AppData, AppError, AppResult, AuthUser, Deserialize, Serialize, Table, TableHeader, User,
};

#[derive(Serialize)]
struct Row {
    pub id: i64,
    pub email: String,
    pub role: String,
    pub created_at: String,
    pub link: String,
}

#[get("/users")]
pub async fn get(data: web::Data<AppData>, user: AuthUser) -> AppResult {
    if user.claims.role != "admin" {
        return Err(AppError::NoAuth);
    }

    let users = sqlx::query_as!(User, "SELECT * FROM users ORDER BY created_at DESC")
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

    Ok(data.render_tpl("users", &table).await)
}

#[get("/users/{id}")]
pub async fn get_user(data: web::Data<AppData>, user: AuthUser, path: web::Path<i64>) -> AppResult {
    if user.claims.role != "admin" {
        return Err(AppError::NoAuth);
    }

    let user_id = path.into_inner();
    let user_data = sqlx::query_as!(User, "SELECT * FROM users WHERE id = ?", user_id)
        .fetch_one(&data.db)
        .await?;

    Ok(data.render_tpl("user", &user_data).await)
}

#[derive(Deserialize)]
pub struct UserUpdateForm {
    pub role: String,
}

#[post("/users/{id}")]
pub async fn post_user(
    data: web::Data<AppData>,
    user: AuthUser,
    path: web::Path<i64>,
    form: web::Form<UserUpdateForm>,
) -> AppResult {
    if user.claims.role != "admin" {
        return Err(AppError::NoAuth);
    }

    let user_id = path.into_inner();

    sqlx::query!("UPDATE users SET role = ? WHERE id = ?", form.role, user_id)
        .execute(&data.db)
        .await?;

    Ok(HttpResponse::Found()
        .append_header(("Location", format!("/users/{}", user_id)))
        .finish())
}

#[delete("/users/{id}")]
pub async fn delete_user(
    data: web::Data<AppData>,
    user: AuthUser,
    path: web::Path<i64>,
) -> AppResult {
    if user.claims.role != "admin" {
        return Err(AppError::NoAuth);
    }

    let user_id = path.into_inner();

    sqlx::query!("DELETE FROM users WHERE id = ?", user_id)
        .execute(&data.db)
        .await?;

    Ok(HttpResponse::Ok().finish())
}
