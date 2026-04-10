use crate::{AppData, AppRole, AuthUser, Data, Responder, Role, get, json};

#[get("/")]
pub async fn index(data: Data<AppData>, user: AuthUser<AppRole>) -> impl Responder {
    data.render_tpl(
        "index",
        &json!({
            "can_read_users": user.claims.role.has_permission("users.read"),
        }),
    )
    .await
}
