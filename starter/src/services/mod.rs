use crate::web;
use full_stack_engine::rate_limiter::auth_rate_limiter;

mod index;
mod login;
mod logout;
mod register;
mod users;

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(index::index);
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
    cfg.service(logout::post);
    cfg.service(users::get);
    cfg.service(users::get_user);
    cfg.service(users::post_user);
    cfg.service(users::delete_user);
}
