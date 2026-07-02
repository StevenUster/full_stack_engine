<div align="center">
  <h1>🛑 UNDER DEVELOPMENT 🛑</h1>
  <p><strong>USE IT AT YOUR OWN RISK</strong></p>
  <img src="https://img.shields.io/badge/STATUS-UNDER_DEVELOPMENT-red?style=for-the-badge&logo=rust" alt="Development Status">
</div>

<br/>

# My Rust Framework

A lightweight, opinionated Rust web framework built on top of Actix-web, SQLx, and Tera.

## Repository Structure

This repository contains two separate Cargo projects:

- **[`/framework`](/framework)**: The core framework code.
- **[`/starter`](/starter)**: A complete template application. Use this to start your own project!

## Features

- **Integrated Auth**: Built-in JWT and Argon2 password hashing. Is also pre-configured in the starter app.
- **Template Engine**: First-class support for Tera templates with an Astro dev server proxy for rapid frontend development.
- **Cron Scheduler**: Easy async job scheduling.
- **Rate Limiting**: proxy-aware, per-client-IP rate limiting via Actix-governor — a generous site-wide limiter is applied to every request automatically (DDoS guard, tunable via `GLOBAL_RATE_LIMIT_*` env vars), plus stricter presets for auth/custom endpoints.
- **Database**: Pre-configured SQLx SQLite pool with automatic migrations.

## Design Principles

These are the core rules the framework is built around. They apply both to changes in `/framework` and to apps built on top of it.

1. **Priorities, in order: Security → Reliability → Speed → Readability.** When two goals conflict, the earlier one wins. A faster or cleaner solution never justifies a weaker security or correctness guarantee.

2. **Secure and stable by default.** Every default the framework ships must be the safest and most robust option available — never the most convenient. If a setting can be insecure, its default is the secure value and loosening it is an explicit, opt-in decision by the app. This includes: autoescaped templates, `HttpOnly` + `SameSite=Strict` + `Secure` (prod) cookies, hardened response headers (CSP, `X-Content-Type-Options`, `X-Frame-Options`), and Argon2 password hashing.

3. **Everything bundles into one executable.** The frontend (`dist/`) and locales are embedded via `include_dir!`, and migrations via `sqlx::migrate!()` (passed to `FrameworkApp::migrator`) — a built binary has no external `migrations/` or `locales/` directory to ship. The framework must not introduce runtime dependencies on external services or sidecar processes. Prefer simple, predictable behavior over configurability for its own sake.

4. **Batteries included — where it makes sense.** Common needs (auth, mail, uploads, i18n, rate limiting, cron, error pages) live in the framework so apps don't re-implement them. A helper earns its place only if most apps want it and it can carry the secure default with it; niche or opinion-heavy concerns stay in the app.

5. **Cross-cutting safety belongs in the framework, not the app.** Security and reliability guarantees that every app needs — token expiry, session/JWT invalidation, proxy-aware rate-limit keying, safe template loading that logs and skips a bad template instead of crashing at boot — should be solved once here so every downstream app inherits them.

6. **Untrusted input stays untrusted.** Parameterize all SQL, validate every upload (size + type), keep public files (`uploads/`) separate from private ones (`data/`), and treat any user-supplied HTML that reaches a renderer as hostile.

7. **Migrations are forward-only.** Schema changes are new, timestamped migrations run automatically at startup; applied migrations are never edited. Apps regenerate the SQLx offline cache (`cargo sqlx prepare`) after query changes.

## Getting Started

The fastest way to get started is to explore the **[Starter App](/starter)**. It comes with a preconfigured frontend (Astro), auth services, and database migrations.

### 1. Copy the Starter
Copy the `starter` folder to your own repository or work directly inside it.

### 2. Configure Environment
Navigate to the starter directory and copy the example environment file:
```bash
cd starter
cp .example.env .env
```

### 3. Run Development Mode
The starter includes a `dev` binary that launches both the Rust backend and the Astro frontend concurrently:
```bash
cargo run --bin dev
```

## License

MIT OR Apache-2.0

