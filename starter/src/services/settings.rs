use crate::{AppData, AppRole, AuthUser, Data, Responder, Role, get, json};

#[get("/settings")]
pub async fn get(data: Data<AppData>, user: AuthUser<AppRole>) -> impl Responder {
    data.render_tpl(
        "settings",
        &json!({
            "can_read_users": user.claims.role.has_permission("users.read"),
        }),
    )
    .await
}
