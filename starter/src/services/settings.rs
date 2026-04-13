use crate::render::AppRenderExt;
use crate::{AppData, AppRole, AuthUser, Data, Responder, get, json};

#[get("/settings")]
pub async fn get(data: Data<AppData>, user: AuthUser<AppRole>) -> impl Responder {
    user.render_tpl(&data, "settings", &json!({})).await
}
