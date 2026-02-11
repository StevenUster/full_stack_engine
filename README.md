# My Rust Framework

A lightweight, opinionated Rust web framework built on top of Actix-web, SQLx, and Tera.

## Features

- **Integrated Auth**: Built-in JWT and Argon2 password hashing.
- **Template Engine**: First-class support for Tera templates with an Astro dev server proxy for rapid frontend development.
- **Cron Scheduler**: Easy async job scheduling.
- **Rate Limiting**: IP-based rate limiting via Actix-governor.
- **Database**: Pre-configured SQLx SQLite pool with automatic migrations.

## Quick Start

The simplest way to use the framework is via the `prelude`, which re-exports common types and traits:

```rust
use crate::include_dir::{include_dir, Dir};
pub use my_rust_framework::prelude::*;

mod cronjobs;
mod services;

static DIST_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/src/frontend/dist");

#[main]
async fn main() -> std::io::Result<()> {
    FrameworkApp::new(&DIST_DIR)
        .configure(services::configure)
        .cronjobs(cronjobs::add_cronjobs)
        .run()
        .await
}
```

## Prelude

The `prelude` module provides a flat namespace for common framework dependencies, ensuring you don't need to add them to your own `Cargo.toml`:

- **Actix Web**: `web`, `HttpRequest`, `HttpResponse`, etc.
- **Database**: `sqlx`, `SqlitePool`.
- **Serialization**: `serde` (Serialize/Deserialize), `serde_json` (json macro).
- **Templates**: `tera` (Context).
- **Framework Core**: `FrameworkApp`, `AppData`, `AuthUser`, `AppResult`, etc.
- **Logging**: `info`, `error`, `debug`, `warn`.

## Environment Variables

See [.example.env](./.example.env) for required environment variables.

