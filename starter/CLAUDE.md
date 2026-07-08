# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

This is the **starter** app for the `full_stack_engine` framework — a template to copy and rename for a new app. The backend is **Rust/Actix-web**, which both serves API routes and renders **Astro/React** frontend templates. SQLite is used for development; PostgreSQL/MariaDB for production. The example domain is a **Products / Orders** manager (a public catalog + admin CRUD + a child "orders" resource) — meant to be renamed to whatever your app actually manages.

## Core Rules

These are the non-negotiable invariants for this codebase. When a change would break one of them, stop and reconsider the approach rather than working around the rule.

**Priorities, in order.** When two goals conflict, resolve in this order: **1. Security → 2. Reliability → 3. Speed/efficiency → 4. Readability.** A faster or prettier solution never justifies a weaker security or correctness guarantee.

**Single, self-contained binary.** Everything static is embedded into the executable at compile time: the frontend (`src/frontend/dist`) and locales (`locales/`) via `include_dir!`, and migrations via `sqlx::migrate!()` (passed to `FrameworkApp::migrator`). A built binary needs no `migrations/` or `locales/` directory beside it. Do not add runtime dependencies on external services, sidecar processes, or files that aren't either embedded or in a mounted volume (`data/`). Prefer simple and predictable over clever.

**Security defaults win.** New settings must default to their safest value; making the insecure/looser option the default is not allowed. Cookies stay `HttpOnly` + `SameSite=Strict` + `Secure` in prod. Never log secrets, tokens, password hashes, or full JWTs.

**Auth is checked in every handler.** Every state-changing endpoint (POST/DELETE) must verify the caller's role before touching data — use `AuthUser::require_permission`. Never trust an ID from the path/form to imply access without also checking ownership where relevant (e.g. a user cancelling their own order). Public read endpoints expose **only** `published` products — never `draft`/`archived` data or user PII.

**The ORM is the only data layer — never write raw SQL.** All database access goes through the fse ORM (re-exported from the framework prelude). Reads/writes use the checked query macros — `find!`, `find_one!`, `find_page!`, `count!`, `insert!`, `update!`, `delete_rows!` — or the generated per-table methods (`Product::fetch`, `Product::fetch_by_slug`, `Product::delete`). These expand to compile-time-checked `sqlx` with bound parameters, so injection is impossible by construction. For a query shape decided at runtime (user-selected sort, optional filters) use the dynamic builder `Product::find().filter(..)`. A `#[orm(relation = fk)] field: Option<Target>` can be eager-loaded with a real SQL JOIN via `include: [field, ...]` on `find!`/`find_one!`; a runtime-shaped join is still fetched as a second query over collected ids (`Table::find().filter(Table::ID.in_(ids))`) and stitched in Rust. If you ever feel you need `sqlx::query!` directly, stop and reconsider the data model instead.

**Template output is escaped by default.** Tera autoescaping is forced on for all templates; only reach for the `safe` filter on values you have escaped yourself. **Anything that lands in `src/frontend/dist/` is registered as a Tera template at boot** — a stray `{{` or `{%` in a `public/` asset crashes startup.

**Tokens are sensitive.** Reset/verification/email-change tokens are single-use secrets — clear them after use and treat expiry as required, not optional.

**Schema lives in `src/tables/`, migrations are generated.** Each `#[derive(Table)]` struct in `src/tables/*.rs` *is* a table; the columns the framework's auth layer needs are protected by `[orm.required_columns]` in `fse.toml`. To change the schema, edit the struct and run `fse migrate` (generates a plain, timestamped sqlx migration into `migrations/`, prints it, applies it). Migrations are forward-only and embedded via `sqlx::migrate!()` — never edit an applied one; hand-written data migrations (seeds/backfills) interleave by timestamp. Run `cargo sqlx prepare` after changing any query so the offline cache (`.sqlx/`) and Docker build stay in sync (`fse migrate` does this for you when a `.sqlx/` dir exists).

## Development Commands

### Rust Backend
```bash
cargo build --release        # production build
cargo run                    # run dev server
cargo watch -x run           # auto-reload on changes
cargo sqlx prepare           # update SQLx query cache after schema changes
cargo run --bin dev          # run backend + frontend together
```

### Frontend (Astro/React) — run from `/src/frontend`
```bash
bun dev          # dev server with HMR
bun run build    # astro check + astro build
bun run preview  # preview built frontend
```

### Database Schema & Migrations
Schema is defined by the `#[derive(Table)]` structs in `src/tables/`. Never
write migration SQL by hand — edit a struct and let the CLI generate it.
```bash
fse migrate            # diff src/tables against the snapshot, generate + apply a migration
fse migrate --dry-run  # show the pending schema change without writing anything
```
`fse` is the ORM CLI (`cargo install --path ../fse-orm/cli`, or `cargo run -p fse-cli --bin fse -- migrate`). A generated migration that needs a data backfill (e.g. a new NOT NULL column) is written with a `TODO` and not applied until you edit it and rerun. Hand-written data migrations (seeds/backfills) can be dropped into `migrations/` and interleave by timestamp.

### Environment Setup
Copy `.example.env` to `.env` and fill in values:
- `ENV` — `dev` or `prod`
- `DATABASE_URL` — SQLite path or postgres/mariadb connection string
- `DOMAIN` / `PROTOCOL` / `PORT` — server bind/host config; used to build absolute URLs
- `JWT_SECRET` — random secret for token signing
- `SMTP_HOST` / `SMTP_USER` / `SMTP_PASS` — mail server credentials (needed for password reset & email verification)
- `EMAIL_VERIFICATION_ENABLED` — `true`/`false`

## Architecture

### Backend (`/src/`)
- **`main.rs`** — Actix-web app setup, route registration, global context injection (user JWT claims, i18n, locales), DB pool via `web::Data<AppData>`
- **`tables/`** — One `#[derive(Table)]` struct per file (`user`, `product`, `order`); these define the schema and generate the ORM's typed queries. This is where you add/rename columns. `DbEnum` types (`ProductStatus`, `OrderStatus`) live beside the table that uses them.
- **`services/`** — One module per route group (`login`, `register`, `forgot_password`, `reset_password`, `logout`, `settings`, `users`, `products`, `orders`, `index`, `api`). Each file exports GET/POST/DELETE handlers. Route registration and rate limiting is in `services/mod.rs`.
- **`services/products.rs`** / **`services/orders.rs`** — The example manageable resource + its child resource. Copy these for your own domain.
- **`services/api.rs`** — Public, unauthenticated, CORS-enabled JSON API for external sites. Exposes only publicly visible data (`published` products). Endpoints: `GET /api/products`, `GET /api/products/{slug}`, plus `GET /api/openapi.json` (OpenAPI 3.0 spec) and `GET /api/docs` (self-hosted Swagger UI).
- **`cronjobs/`** — Scheduled tasks (currently empty scaffolding)
- **`bin/`** — Dev runner (`dev.rs`), password hashing (`hash_password.rs`), email testing (`test_email.rs`)

### Frontend (`/src/frontend/src/`)
- **`pages/`** — Astro pages mapped to routes. Subdirectories: `product-manager/` (admin product CRUD, tabbed detail view), `products/` (public catalog + detail), `public/` (public-facing error/no-access pages), `emails/` (email templates), `api/` (Swagger UI docs page)
- **`layouts/`** — Main layout with sidebar, responsive shell, light/dark mode
- **`components/`** — Reusable Astro/React components (`Table`, `TableFilters`, `Pagination`, `SearchBar`, `Card`, `Modal`, `Select`, `ProductTabs`) — kept domain-agnostic on purpose; copy `ProductTabs.astro` for any resource that needs a tabbed detail view
- Tailwind CSS with custom brand colors defined in `src/styles/global.css` (primary green `#92c355`)

### Auth & Roles
- JWT-based auth via the `full_stack_engine` crate (internal custom framework)
- Roles: `Admin` (all permissions), `Manager` (`users.read/write`, `products.read/write`), `User`, `None`
- Route guards check permissions in service handlers; rate limiting applied to `login`, `register`, `forgot-password`, `reset-password`

### Database Schema (key entities — defined in `src/tables/`)
- **Users** (`user.rs`) — email/password/role, first/last name, email verification + password reset + email change tokens (all with expiry), `sessions_valid_after` for server-side session revocation. The framework's auth layer requires the columns listed in `fse.toml` `[orm.required_columns]`.
- **Products** (`product.rs`) — name, slug, description, price, `status` enum (`draft`, `published`, `archived` — only `published` is publicly visible)
- **Orders** (`order.rs`) — product_id, user_id (both FK + indexed), quantity, note, `status` enum (`pending`, `fulfilled`, `cancelled`)

### Localization
- `/locales/` — JSON files per language: `en.json` (default) and `de.json`
- Locale data is loaded in `lib.rs` and injected into every template context

## Deployment

Multi-stage Dockerfile: Bun builds the frontend → Rust compiles the backend → Debian slim runtime image on port 8080. CI/CD via GitHub Actions pushes to `ghcr.io` (`.github/workflows/docker-publish.yml`). SQLite data is persisted via a Docker volume.
