# Starter

A starter app for the `full_stack_engine` framework: Rust/Actix-web backend + Astro/React frontend, showcasing auth (login/register/email verification/password reset/settings), role-based permissions, a public JSON API with self-hosted Swagger docs, and a generic **Products / Orders** manager UI (tabs, tables with search/filter/pagination, modals) meant to be copied and renamed for your own domain.

---

## Core Principles

The rules every change to this app should respect:

1. **Priorities, in order:** Security → Reliability → Speed → Readability. When they conflict, the earlier one wins.
2. **One self-contained binary.** The Astro frontend and locales are embedded with `include_dir!`, and migrations with `sqlx::migrate!()` — a built binary carries them all and needs no `migrations/` or `locales/` directory beside it. No sidecar processes; persistent state lives only in the `data/` volume. Keep it simple and predictable.
3. **Secure by default.** New options default to their safest value. Auth cookies stay `HttpOnly` + `SameSite=Strict` + `Secure` (prod). Secrets and tokens are never logged.
4. **Every handler authorizes.** State-changing endpoints check role before acting. The public site and API expose only `published` products — never drafts/archived data or extra user PII.
5. **Parameterized SQL only.** All user input goes through `sqlx` bind parameters; never string-formatted into a query.
6. **Escaped templates.** Tera autoescaping is always on; `safe` is used only on pre-escaped values. Anything placed in `src/frontend/dist/` becomes a Tera template at boot, so raw `{{ }}` in static assets will crash startup.
7. **Forward-only migrations.** Add a new timestamped migration per schema change and run `cargo sqlx prepare` afterwards; never edit an applied migration.

See [`CLAUDE.md`](./CLAUDE.md) for the detailed rationale behind each rule.

---

## Features

### Auth & users

- Role-based permissions: `Admin`, `Manager`, `User`, `None`.
- Email verification for new accounts (optional, toggled via `EMAIL_VERIFICATION_ENABLED`).
- Password reset via email.
- Email address changes with re-verification.
- Admin user management UI (`/users`) with role assignment.

### Products (example manageable resource)

- Public catalog (`/products`) and detail pages — only `published` products are visible.
- Admin CRUD (`/product-manager`) with search, filters, and pagination.
- A tabbed detail view (Overview / Orders) — copy `ProductTabs.astro` for any resource with more than one sub-view.

### Orders (example child resource)

- Signed-in users place orders against a product.
- Managers view and fulfil orders from the product's Orders tab.
- Users can cancel their own pending orders from `/my-orders`.

### Public API

- Read-only, unauthenticated, CORS-enabled JSON API (`/api/products`, `/api/products/{slug}`).
- Self-hosted Swagger UI at `/api/docs`, spec at `/api/openapi.json` — no external/CDN requests.

### Frontend

- Astro + React, Tailwind CSS, light/dark mode.
- Bilingual (English default, German) via `locales/en.json` / `locales/de.json`.

---

## Deployment

Prepare sqlx queries:

```bash
cargo sqlx prepare
```

Build the image:

```bash
VERSION=$(grep "^version =" Cargo.toml | cut -d '"' -f 2)
podman build -t ghcr.io/stevenuster/full_stack_engine:latest -t ghcr.io/stevenuster/full_stack_engine:$VERSION .
```

Push the image to ghcr.io:

```bash
VERSION=$(grep "^version =" Cargo.toml | cut -d '"' -f 2)
podman push ghcr.io/stevenuster/full_stack_engine:latest
podman push ghcr.io/stevenuster/full_stack_engine:$VERSION
```

## Development

### Hot Reloading (Dev Mode)

**Prerequisites:**

- [Bun](https://bun.sh/) - JavaScript runtime for the Astro frontend
- `cargo-watch` - For Rust auto-reloading

Install the required tools:

```bash
curl -fsSL https://bun.sh/install | bash

cargo install cargo-watch
```

Make sure your `.env` file has:

```
ENV=dev
```

Then run the development server (starts both Rust backend and Astro frontend):

```bash
cargo run --bin dev
```

Press Ctrl+C to stop both servers

Alternatively, run them separately in two terminals:

```bash
# Terminal 1: Rust backend with hot reload
cargo watch -x run

# Terminal 2: Astro frontend dev server
cd src/frontend
bun dev
```

### Database migrations

Install sqlx cli if you don't have it:

```bash
cargo install sqlx-cli --no-default-features --features sqlite
```

Add a new migration:

```bash
sqlx migrate add migration_name
```

Execute all migrations that haven't been applied:

```bash
sqlx migrate run
```

_Note: Before the webserver starts all migrations are run to ensure that the database has everything in production._

## Keep everything up to date

### Rust toolchain

```bash
rustup self update
```

```bash
rustup update stable
```

### Bun

```bash
bun upgrade
```

### NPM Packages

```bash
bun update
```

### Astro

```bash
bun x @astrojs/upgrade
```

### Rust crates

```bash
cargo install cargo-edit
cargo upgrade
```
