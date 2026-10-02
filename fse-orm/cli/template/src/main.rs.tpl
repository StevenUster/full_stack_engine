#![deny(warnings, clippy::all, clippy::pedantic)]

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    __CRATE__::run().await
}
