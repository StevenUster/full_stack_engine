<div align="center">
  <h1>🛑 UNDER DEVELOPMENT 🛑</h1>
  <p><strong>USE IT AT YOUR OWN RISK</strong></p>
  <img src="https://img.shields.io/badge/STATUS-UNDER_DEVELOPMENT-red?style=for-the-badge&logo=rust" alt="Development Status">
</div>

<br/>

# My Rust Framework

A lightweight, opinionated Rust web framework built on top of Actix-web, SQLx, and Tera.

## Repository Structure

This repository is organized as a Cargo Workspace:

- **[`/framework`](./framework)**: The core framework code.
- **[`/starter`](./starter)**: A complete template application. Use this to start your own project!

## Features

- **Integrated Auth**: Built-in JWT and Argon2 password hashing. Is also pre-configured in the starter app.
- **Template Engine**: First-class support for Tera templates with an Astro dev server proxy for rapid frontend development.
- **Cron Scheduler**: Easy async job scheduling.
- **Rate Limiting**: IP-based rate limiting via Actix-governor.
- **Database**: Pre-configured SQLx SQLite pool with automatic migrations.

## Getting Started

The fastest way to get started is to explore the **[Starter App](./starter)**. It comes with a preconfigured frontend (Astro), auth services, and database migrations.

### 1. Copy the Starter
Copy the `starter` folder to your own repository or work directly inside it.

### 2. Configure Environment
Copy the example environment file in the starter:
```bash
cp starter/example.env starter/.env
```

### 3. Run Development Mode
The starter includes a `dev` binary that launches both the Rust backend and the Astro frontend concurrently:
```bash
cargo run --bin dev
```

## License

MIT OR Apache-2.0

