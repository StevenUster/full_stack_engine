# Starter

The reference app for **full_stack_engine** — what a finished app looks
like: almost everything is declared by the `#[model]` structs in
`src/models/`; `src/services/` holds only the one flow a model can't express.

**Agents and developers: read [`AGENTS.md`](./AGENTS.md)** — the complete
guide (workflow, every model option and hook, security rules, testing).
For a blank new app instead of this showcase, run `fse new <name>`.

```bash
cp .example.env .env        # set JWT_SECRET (≥ 32 chars)
(cd theme && bun install && bun run build)
fse migrate
cargo test
cargo run --bin dev         # backend + theme dev server
```

---

## What's in it

| feature | how |
|---|---|
| Auth: login, registration, email verification, password reset, settings, user admin (`/users`) | `.module(auth_module)` — no auth code in the app |
| Roles `Admin`, `Manager`, `User`, `None` | `define_roles!` in `src/lib.rs` |
| Products admin `/admin/products`: search, status filter, validation (`min`, `max`), slug derived from the name, formatted price/date | `src/models/product.rs` |
| Public catalog `/products`, `/products/{slug}` and JSON API `/api/products` — published products only | `public_read` + `api` + the `public_scope` hook on `Product` |
| Orders admin `/admin/orders`: product and customer by name, *Fulfill*/*Cancel* buttons on pending orders | `src/models/order.rs` — relations with `#[ui(list)]`, `actions(...)` + `can_act` |
| Customers order a published product, see `/my-orders`, cancel their own pending order | `src/services/orders.rs` — the custom-flow example |
| OpenAPI document + Swagger UI at `/api/docs` | `.api_docs(...)`, generated from the `api` models |
| Child theme: own catalog pages, extra sidebar links, recolored palette | `theme/` (parent: `fse-theme-default`) |
| English + German | `locales/*.json` (framework texts built in) |
| Tests over the production stack | `tests/` with `TestApp` |

Run `cargo run -- --routes` to see every generated route and the permission
it needs.

---

## Deployment

Commit the query cache (`fse migrate` refreshes it; `fse prepare` after
editing a query only):

```bash
fse prepare
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

Then run the development server (starts both the Rust backend and the theme's Astro dev server; builds `theme/` once if it has never been built):

```bash
cargo run --bin dev
```

Press Ctrl+C to stop both servers

Alternatively, run them separately in two terminals:

```bash
# Terminal 1: Rust backend with hot reload
cargo watch -x run

# Terminal 2: theme dev server
cd theme
bun dev
```

### Database migrations

Never written by hand. Change a struct in `src/models/`, then:

```bash
cargo install fse-cli      # once
fse migrate                # diff the models, write + apply a migration, refresh .sqlx/
fse migrate --dry-run      # preview only
```

Pending migrations also run automatically when the app boots.

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
