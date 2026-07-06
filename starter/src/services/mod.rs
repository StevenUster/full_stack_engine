use crate::web;
use full_stack_engine::rate_limiter::{auth_rate_limiter, custom_rate_limiter};

pub use full_stack_engine::prelude::RenderTplExt;

pub mod api;
pub mod forgot_password;
pub mod index;
pub mod login;
pub mod logout;
pub mod orders;
pub mod products;
pub mod register;
pub mod reset_password;
pub mod settings;
pub mod users;

pub(super) fn load_locale(lang: &str) -> crate::serde_json::Value {
    full_stack_engine::i18n::load_locale(&crate::LOCALES_DIR, lang)
}

/// How long a single-use email token (password reset, email verification,
/// email change) stays valid after it is issued.
const TOKEN_TTL_HOURS: i64 = 24;

/// Expiry timestamp for a freshly issued email token (UTC, like everything
/// the ORM writes into TIMESTAMP columns).
pub(super) fn token_expiry() -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc() + chrono::Duration::hours(TOKEN_TTL_HOURS)
}

/// Current UTC time, for comparing against TIMESTAMP columns (token expiry
/// checks in `find_one!`/`update!` filters).
pub(super) fn now() -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

/// Current unix time, used to stamp `users.sessions_valid_after` when existing
/// sessions must be invalidated (role change, password reset, ...).
pub(super) fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
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
    cfg.service(forgot_password::get);
    cfg.service(
        // One request per hour per IP: this sends an email, so it needs a
        // stricter limit than the general auth endpoints.
        web::resource("/forgot-password")
            .route(web::post().to(forgot_password::post))
            .wrap(custom_rate_limiter(3600, 1)),
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
    cfg.service(api::get_docs);
    cfg.service(api::get_openapi_spec);
    cfg.service(api::get_products);
    cfg.service(api::get_product_detail);
    cfg.service(products::get_public_products);
    cfg.service(products::get_public_product_detail);
    cfg.service(products::get_products);
    cfg.service(products::get_product_create);
    cfg.service(products::post_product_create);
    cfg.service(products::get_product);
    cfg.service(products::post_product);
    cfg.service(products::delete_product);
    cfg.service(orders::post_place_order);
    cfg.service(orders::get_my_orders);
    cfg.service(orders::post_cancel_my_order);
    cfg.service(orders::get_product_orders);
    cfg.service(orders::post_fulfill_order);
    cfg.service(orders::delete_order);
    cfg.service(reset_password::get);
    cfg.service(
        web::resource("/reset-password")
            .route(web::post().to(reset_password::post))
            .wrap(auth_rate_limiter()),
    );
}
