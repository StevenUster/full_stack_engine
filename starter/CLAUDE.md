# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

This is the **starter** app for the `full_stack_engine` framework — a template to copy and rename for a new app. **The app is defined by the `#[model]` structs in `src/models/`**: each struct generates its table (migrations via `fse migrate`), its compile-time-checked ORM queries, and its admin CRUD endpoints + pages (mounted by `.models::<AppRole>()`). Auth (login/register/email verification/password reset/settings/user admin) comes from the framework's built-in auth module; the visual layer comes from the `fse-theme-default` theme. What remains hand-written is **overrides and custom flows only**.

The example domain: a `Product` catalog (generated admin at `/admin/products`, hand-written public pages at `/products` showing only `published` rows) and an `Order` child resource (generated admin at `/admin/orders`, hand-written user flows for placing/cancelling own orders).

## Core Rules

**Priorities, in order:** 1. Security → 2. Reliability → 3. Speed/efficiency → 4. Readability. A faster or prettier solution never justifies a weaker security or correctness guarantee.

**Models are the app.** To add a CRUD feature, add one `#[model]` struct in `src/models/` and run `fse migrate` — endpoints, pages, permissions (`{table}.read`/`{table}.write`) and translations exist after that. Configure with `#[model(...)]` (permission, path, public_read, api, no_create/no_edit/no_delete, disabled, title_field) and per-field `#[ui(...)]` (list, search, filter, textarea, hidden, readonly). Everything is validated at compile time.

**Override, don't fork.** Registration order is the override mechanism — app routes (`services/`, registered first) beat auth-module routes beat generated routes on the same path. A page is overridden by creating a same-path file under `src/frontend/src/pages/` (beats theme and module pages). Generated handlers prefer a model-specific template (`admin/products`, `admin/products/form`) over the theme's generic `fse/*` ones. Write an override only for what generation can't know (business rules like "only `published` products are public") — see `services/products_public.rs` and `services/orders.rs` for the canonical examples.

**Single, self-contained binary.** Frontend dist and locales are embedded via `include_dir!`, migrations via `sqlx::migrate!()`. No runtime dependencies on external services or non-volume files.

**Security defaults win.** Cookies stay `HttpOnly` + `SameSite=Strict` + `Secure` in prod. Never log secrets, tokens, password hashes, or full JWTs. Every hand-written state-changing endpoint verifies the caller's role (`AuthUser::require_permission`) and ownership where relevant; generated endpoints do this by convention. Public read endpoints expose **only** `published` products.

**The ORM is the only data layer in app code — never write raw SQL.** Reads/writes use the checked query macros (`find!`, `find_one!`, `find_page!`, `count!`, `insert!`, `update!`, `delete_rows!`), the generated per-table methods, or the dynamic builder (`Product::find().filter(..)`) for runtime-shaped queries.

**Template output is escaped by default.** Tera autoescaping is forced on; anything in `src/frontend/dist/` is registered as a Tera template at boot (template names are used verbatim — no rewriting).

**Schema lives in `src/models/`, migrations are generated.** Edit a struct, run `fse migrate`. The auth module's `users` columns are protected by `[orm.required_columns]` in `fse.toml`. Migrations are forward-only; never edit an applied one.

## Development Commands

### Rust Backend
```bash
cargo run --bin dev          # run backend + frontend dev servers together
cargo run                    # backend only
cargo test                   # integration tests (incl. every template render-checked)
```

### Frontend (Astro) — run from `src/frontend`
```bash
bun dev          # dev server with HMR
bun run build    # astro check + astro build (required before a release build)
```

### Database Schema & Migrations
```bash
fse migrate            # diff src/models against the snapshot, generate + apply a migration
fse migrate --dry-run  # show the pending change without writing
fse sync               # extract configured module frontends into .fse/modules/
```
`fse` is the ORM CLI (`cargo install fse-cli`).

## Architecture

### Backend (`src/`)
- **`models/`** — THE APP. One `#[model]` struct per file. This is where features start.
- **`services/`** — overrides and custom flows only: `products_public.rs` (published-only catalog), `orders.rs` (place/my-orders/cancel-own), `api.rs` (public JSON API + OpenAPI/Swagger), `index.rs`. Registered in `services/mod.rs`, always before modules/generated routes.
- **`lib.rs`** — roles (`define_roles!`), the builder chain (`configure` → `module(auth)` → `models::<AppRole>()` → `locales(...)`), context injector for user claims.
- Auth flows, settings and user admin come from `full_stack_engine::auth_module` — override any of its routes/pages the same way as generated ones.

### Frontend (`src/frontend/src/`)
- **`pages/`** — app pages and overrides only (public products, my-orders, index, error pages, API docs). Login/register/settings/users/admin CRUD pages come from `fse-theme-default` (override by creating the same path here).
- **`styles/global.css`** — the single Tailwind root: imports `tailwindcss`, then the theme's design layer, then `@source`s the theme package. App-specific tokens go here.
- **`components/`, `layouts/`** — the app's own chrome (Sidebar etc.). The theme ships its own set for its pages.

### Localization
- `locales/en.json` + `de.json` hold **app-specific keys only**: `models.{table}` labels for generated UIs, plus sections the app's own pages use. The framework provides all auth/CRUD-chrome translations; app keys deep-merge on top (app wins).
- Language selection is one of three modes in `lib.rs`: `Hardcoded`, `Domain` (host → language) or `Path` (`/de/...`, default language unprefixed).

### Auth & Roles
- Roles in `lib.rs`: `Admin` (all), `Manager` (users/products/orders read+write), `User`, `None`. Generated endpoints check `{table}.read`/`{table}.write`; the auth module's user admin checks `users.read`/`users.write` with admin-only escalation guards.
- Self-registered accounts get the role named `"user"`.

## Deployment

Multi-stage Dockerfile: Bun builds the frontend → Rust compiles the backend → slim runtime image. SQLite data persists via the `data/` volume.
