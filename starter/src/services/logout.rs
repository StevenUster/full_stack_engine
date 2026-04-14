use crate::{
    AppData, AppRole, AuthUser, Data, Env, HttpResponse, LOCATION, Responder, cookie::Cookie,
    cookie::time::Duration, post,
};

#[post("/logout")]
pub async fn post(data: Data<AppData>, _user: AuthUser<AppRole>) -> impl Responder {
    let cookie = Cookie::build("token", "")
        .domain(&data.domain)
        .path("/")
        .same_site(actix_web::cookie::SameSite::Strict)
        .secure(data.env != Env::Dev)
        .http_only(true)
        .max_age(Duration::seconds(0))
        .finish();

    HttpResponse::SeeOther()
        .append_header((LOCATION, "/login"))
        .cookie(cookie)
        .finish()
}
