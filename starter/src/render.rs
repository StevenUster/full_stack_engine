use actix_web::HttpResponse;
use actix_web::web::Data;
use full_stack_engine::prelude::*;
use full_stack_engine::render::DefaultRenderExt;
use serde::Serialize;
use serde_json::json;

pub trait AppRenderExt: DefaultRenderExt {
    fn render_tpl<T: Serialize + Send + Sync>(
        &self,
        data: &Data<AppData>,
        template: &str,
        context: &T,
    ) -> impl std::future::Future<Output = HttpResponse> + Send;
}

// // This is an example of how to add global context variables to all templates
// // Use this to add variables that should be available in all templates
impl<U: DefaultRenderExt + Sync> AppRenderExt for U {
    async fn render_tpl<T: Serialize + Send + Sync>(
        &self,
        data: &Data<AppData>,
        template: &str,
        context: &T,
    ) -> HttpResponse {
        let mut value = serde_json::to_value(context).unwrap_or_else(|_| json!({}));
        if let Some(obj) = value.as_object_mut() {
            obj.insert("is_demo_mode".to_string(), json!(true));
            obj.insert("theme_color".to_string(), json!("blue"));
        }
        DefaultRenderExt::render_tpl(self, data, template, &value).await
    }
}
