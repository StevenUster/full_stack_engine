use crate::render::AppRenderExt;
use crate::{AppData, AppRole, AuthUser, Data, Responder, get, json};

#[get("/")]
pub async fn index(data: Data<AppData>, user: AuthUser<AppRole>) -> impl Responder {
    user.render_tpl(&data, "index", &json!({})).await
}
