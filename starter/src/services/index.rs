use crate::{get, AppData, AuthUser, Data, Responder};

#[get("/")]
pub async fn index(data: Data<AppData>, _user: AuthUser) -> impl Responder {
    data.render("index").await
}
