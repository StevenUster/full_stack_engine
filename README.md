# My Rust Framework

A lightweight, opinionated Rust web framework built on top of Actix-web, SQLx, and Tera.

## Features

- **Integrated Auth**: Built-in JWT and Argon2 password hashing.
- **Template Engine**: First-class support for Tera templates with an Astro dev server proxy for rapid frontend development.
- **Cron Scheduler**: Easy async job scheduling.
- **Rate Limiting**: IP-based rate limiting via Actix-governor.
- **Database**: Pre-configured SQLx SQLite pool with automatic migrations.

## Quick Start

```rust
use my_rust_framework::{FrameworkApp, include_dir::{Dir, include_dir}};

// Include your frontend dist directory
static DIST_DIR: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/frontend/dist");

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    FrameworkApp::new(&DIST_DIR)
        .configure(|cfg| {
            // Register your services here
            // cfg.service(web::resource("/").to(index));
        })
        .cronjobs(|sched, db| async move {
            // Setup cron jobs here
            Ok(())
        })
        .run()
        .await
}
```

## Environment Variables

See [.example.env](./.example.env) for required environment variables.

