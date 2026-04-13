use crate::AppData;
use crate::auth::AuthUser;
use crate::structs::Role;
use actix_web::HttpResponse;
use actix_web::web::Data;
use serde::Serialize;
use serde_json::json;

pub trait DefaultRenderExt {
    fn render_tpl<T: Serialize + Send + Sync>(
        &self,
        data: &Data<AppData>,
        template: &str,
        context: &T,
    ) -> impl std::future::Future<Output = HttpResponse> + Send;
}

impl<R: Role + Serialize + Send + Sync> DefaultRenderExt for AuthUser<R> {
    async fn render_tpl<T: Serialize + Send + Sync>(
        &self,
        data: &Data<AppData>,
        template: &str,
        context: &T,
    ) -> HttpResponse {
        let mut value = serde_json::to_value(context).unwrap_or_else(|_| json!({}));
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "can_read_users".to_string(),
                serde_json::json!(self.claims.role.has_permission("users.read")),
            );
            obj.insert(
                "user".to_string(),
                serde_json::to_value(&self.claims).unwrap_or(serde_json::json!({})),
            );
        }
        data.render_tpl(template, &value).await
    }
}
