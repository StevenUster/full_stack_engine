#![deny(warnings, unused_imports, dead_code, clippy::all, clippy::pedantic)]

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    starter::run().await
}
