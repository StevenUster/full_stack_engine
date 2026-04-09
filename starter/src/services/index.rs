use crate::{AppData, AuthUser, Data, DefaultRole, Responder, get, json};

#[get("/")]
pub async fn index(data: Data<AppData>, user: AuthUser<DefaultRole>) -> impl Responder {
    data.render_tpl("index", &json!({ "role": user.claims.role.to_string() }))
        .await
}
