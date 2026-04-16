use crate::{Responder, get, json};

#[get("/")]
pub async fn index(req: actix_web::HttpRequest) -> impl Responder {
    use super::RenderTplExt;
    req.render_tpl("index", &json!({})).await
}
