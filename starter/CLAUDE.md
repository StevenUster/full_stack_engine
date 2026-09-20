# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Overview

This is the **starter** app for the `full_stack_engine` framework — a template to copy and rename for a new app. **The app is defined by the `#[model]` structs in `src/models/`**: each struct generates its table (migrations via `fse migrate`), its compile-time-checked ORM queries, and its admin CRUD endpoints + pages (mounted by `.models::<AppRole>()`). Auth (login/register/email verification/password reset/settings/user admin) comes from the framework's built-in auth module; the visual layer comes from the `fse-theme-default` theme, extended by the app's child theme in `theme/`. What remains hand-written is **overrides and custom flows only**.

The example domain: a `Product` catalog (generated admin at `/admin/products`, hand-written public pages at `/products` showing only `published` rows) and an `Order` child resource (generated admin at `/admin/orders`, hand-written user flows for placing/cancelling own orders).

## Core Rules

**Priorities, in order:** 1. Security → 2. Reliability → 3. Speed/efficiency → 4. Readability. A faster or prettier solution never justifies a weaker security or correctness guarantee.

**Models are the app.** To add a CRUD feature, add one `#[model]` struct in `src/models/` and run `fse migrate` — endpoints, pages, permissions (`{table}.read`/`{table}.write`) and translations exist after that. Configure with `#[model(...)]` (permission, path, public_read, api, no_create/no_edit/no_delete, disabled, title_field) and per-field `#[ui(...)]` (list, search, filter, textarea, hidden, readonly). Everything is validated at compile time.

**Override, don't fork.** Registration order is the override mechanism — app routes (`services/`, registered first) beat auth-module routes beat generated routes on the same path. A page is overridden by creating a same-path file under `theme/src/pages/` (beats parent-theme and module pages); any parent component/layout/style is overridden by creating the same `src/` path in `theme/`. Generated handlers prefer a model-specific template (`admin/products`, `admin/products/form`) over the theme's generic `fse/*` ones. Write an override only for what generation can't know (business rules like "only `published` products are public") — see `services/products_public.rs` and `services/orders.rs` for the canonical examples.

**Single, self-contained binary.** Both themes (`fse_theme_default::DIST`, `theme/dist`) and locales are embedded via `include_dir!`, migrations via `sqlx::migrate!()`. No runtime dependencies on external services or non-volume files.

**Security defaults win.** Cookies stay `HttpOnly` + `SameSite=Strict` + `Secure` in prod. Never log secrets, tokens, password hashes, or full JWTs — and note that the framework's request span deliberately records `url.path` but **never** the query string, because auth links carry single-use tokens there (`/reset-password?token=…`); don't add a field that reintroduces one, and don't put a secret in a path segment. Every hand-written state-changing endpoint verifies the caller's role (`AuthUser::require_permission`) and ownership where relevant; generated endpoints do this by convention. Public read endpoints expose **only** `published` products.

**Configuration is read once, at boot.** Everything the app needs from the environment is validated by `full_stack_engine::config::Config` before the server starts, and *every* problem is reported together — never add an `env::var(...).expect(...)` in a handler or a helper. Secrets (`JWT_SECRET`, `SMTP_PASS`) are `SecretString`: read them with `data.jwt_secret()` / `.expose_secret()`, never store them in a plain `String`, and never put them in a struct that derives `Debug`. SMTP must be set as all three variables or none, and `EMAIL_VERIFICATION_ENABLED=true` without SMTP fails the boot on purpose.

**Observability is configured, not called.** Log with the prelude's `info!`/`warn!`/`error!` (these are `tracing`'s macros — structured fields work: `info!(order.id = id, "order placed")`), return an `AppError`, and stop. The framework opens one span per request, logs each failure exactly once with its full cause chain, echoes a correlation id as `x-request-id`, and forwards to OTLP/Sentry when those are configured. Never call a vendor SDK from a handler. When wrapping a foreign error, use `.context("…")` rather than `AppError::Internal(format!("…: {e}"))` — the former keeps the cause reachable via `source()`. See [../docs/observability.md](../docs/observability.md).

**Don't undo the response pipeline.** The framework compresses every response, serves fingerprinted `_astro/` assets as immutable, and in production replaces `script-src 'unsafe-inline'` with a per-request nonce that it stamps onto every `<script>` tag. Two things follow: never add an inline event handler (`onclick=`) to a template — a nonce cannot cover it and it will be blocked — and never put another language's translations (or any bulk data) into a page's render context, since the whole context ships to the client in `__fse-props__`. `tests/page_snapshots.rs` snapshots that payload; if it grows, that's the review signal.

**Revoking a session means calling `revoke_sessions`.** The `sessions_valid_after` lookup is cached for 5 seconds, so writing that column with raw SQL leaves outstanding tokens working until the entry expires. Use `full_stack_engine::auth::revoke_sessions(db, user_id)`, or — if the write has to be part of a larger atomic statement — call `auth::invalidate_session_cache(user_id)` immediately after it.

**The ORM is the only data layer in app code — never write raw SQL.** Reads/writes use the checked query macros (`find!`, `find_one!`, `find_page!`, `count!`, `insert!`, `update!`, `delete_rows!`), the generated per-table methods, or the dynamic builder (`Product::find().filter(..)`) for runtime-shaped queries.

**Template output is escaped by default.** Tera autoescaping is forced on; every `.html` file of the theme stack is registered as a Tera template at boot (child over parent; template names are used verbatim).

**Schema lives in `src/models/`, migrations are generated.** Edit a struct, run `fse migrate`. The auth module's `users` columns are protected by `[orm.required_columns]` in `fse.toml`. Migrations are forward-only; never edit an applied one.

## Development Commands

### Rust Backend
```bash
cargo run --bin dev          # run backend + frontend dev servers together
cargo run                    # backend only
cargo test                   # integration tests (incl. every template render-checked)

cargo deny check                                   # advisories, licences, no-OpenSSL
cargo insta review                                 # accept intended page-snapshot changes

LOG_FORMAT=json cargo run                         # see what prod will emit
RUST_LOG=sqlx=debug cargo run                      # every SQL statement
RUST_LOG=full_stack_engine::access=off cargo run   # drop the per-request access log
```

### Theme (Astro) — run from `theme/`
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
- **`lib.rs`** — roles (`define_roles!`), `themes()`, the builder chain (`theme`s → `configure` → `module(auth)` → `models::<AppRole>()` → `locales(...)`), context injector for app extras (the framework injects `nav`/`user`).
- Auth flows, settings and user admin come from `full_stack_engine::auth_module` — override any of its routes/pages the same way as generated ones.

### Theme (`theme/`) — a child theme of `fse-theme-default` (see `../docs/themes.md`)
- **`theme.json`** — `{ "name": "starter", "parent": "fse-theme-default" }`. `src/lib.rs::themes()` installs the parent (crate) and this theme's built `dist/`; the child is active, the parent fills in anything missing at runtime.
- **`src/pages/`** — the app's own pages only (public products, my-orders, API docs). Home, error, login/register/settings/users, emails and admin CRUD pages are inherited from the parent (built into `dist/` by fse-ssr with this theme's overrides applied).
- **`src/components/SidebarLinks.astro`, `src/styles/global.css`** — override examples: a parent file at the same `src/` path is replaced everywhere it's used. Import parent originals with `@parent/...`.

### Localization
- `locales/en.json` + `de.json` hold **app-specific keys only**: `models.{table}` labels for generated UIs, plus sections the app's own pages use. The framework provides all auth/CRUD-chrome translations; app keys deep-merge on top (app wins).
- Language selection is one of three modes in `lib.rs`: `Hardcoded`, `Domain` (host → language) or `Path` (`/de/...`, default language unprefixed).

### Auth & Roles
- Roles in `lib.rs`: `Admin` (all), `Manager` (users/products/orders read+write), `User`, `None`. Generated endpoints check `{table}.read`/`{table}.write`; the auth module's user admin checks `users.read`/`users.write` with admin-only escalation guards.
- Self-registered accounts get the role named `"user"`.

## Deployment

Multi-stage Dockerfile: Bun builds the child theme → Rust compiles the backend → slim runtime image. SQLite data persists via the `data/` volume.

Logs go to stdout as JSON for the container runtime to collect. Set `SERVICE_VERSION` to the deployed commit SHA (two builds of `0.1.0` are not the same binary), and `OTEL_EXPORTER_OTLP_ENDPOINT` / `SENTRY_DSN` if those backends exist — every variable is listed in `.example.env` and passed through by `docker-compose.yml`.
