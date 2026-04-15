use crate::web;
use full_stack_engine::rate_limiter::auth_rate_limiter;

mod index;
mod login;
mod logout;
mod register;
mod reset_password;
mod settings;
mod users;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(index::index);
    cfg.service(settings::get);
    cfg.service(settings::post_change_email);
    cfg.service(settings::verify_email_change);
    cfg.service(settings::post_password_reset);
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
}
