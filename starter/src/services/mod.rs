use crate::web;
use full_stack_engine::rate_limiter::{auth_rate_limiter, custom_rate_limiter};

pub use full_stack_engine::prelude::RenderTplExt;

mod api;
mod forgot_password;
mod index;
mod login;
mod logout;
mod register;
mod reset_password;
mod settings;
#[cfg(test)]
mod tests;
mod users;

/// Loads `locales/<lang>.json`, for one-off lookups (e.g. an email subject)
/// outside of the global template context set up in `main.rs`.
pub(super) fn load_locale(lang: &str) -> crate::serde_json::Value {
    full_stack_engine::i18n::load_locale(&crate::LOCALES_DIR, lang)
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(index::index);
    cfg.service(settings::get);
    cfg.service(settings::post_change_email);
    cfg.service(settings::verify_email_change);
    cfg.service(settings::post_password_reset);
    cfg.service(settings::post_delete_account);
    cfg.service(login::get);
    cfg.service(
        web::resource("/login")
            .route(web::post().to(login::post))
            .wrap(auth_rate_limiter()),
    );
    cfg.service(register::get);
    cfg.service(
        web::resource("/register")
            .route(web::post().to(register::post))
            .wrap(auth_rate_limiter()),
    );
    cfg.service(register::verify_email);
    cfg.service(register::register_success);
    cfg.service(logout::get);
    cfg.service(logout::post);
    cfg.service(users::get);
    cfg.service(users::get_user);
    cfg.service(users::post_user);
    cfg.service(users::delete_user);
    cfg.service(reset_password::get);
    cfg.service(
        web::resource("/reset-password")
            .route(web::post().to(reset_password::post))
            .wrap(auth_rate_limiter()),
    );
    cfg.service(forgot_password::get);
    cfg.service(
        // One request per hour per IP: this sends an email, so it needs a
        // stricter limit than the general auth endpoints.
        web::resource("/forgot-password")
            .route(web::post().to(forgot_password::post))
            .wrap(custom_rate_limiter(3600, 1)),
    );
    cfg.service(api::get_docs);
    cfg.service(api::get_openapi_spec);
}
