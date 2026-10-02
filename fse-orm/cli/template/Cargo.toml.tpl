[package]
name = "__NAME__"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
__DEPS__
# Needed only because these crates' macros expand to absolute `::crate::` paths
# (actix-web's #[get], serde's derive, sqlx::migrate!). Features come from the
# framework.
actix-web = "4"
serde = { version = "1", features = ["derive"] }
sqlx = { version = "0.8", default-features = false }

[profile.release]
lto = true
codegen-units = 1
opt-level = 3
