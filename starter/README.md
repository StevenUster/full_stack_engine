# Starter

## Deployment

Prepare sqlx queries:

```bash
cargo sqlx prepare
```

Build the image

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

### Server-rendered data in templates (fse-ssr)

Pages never contain Tera syntax. Server data is used like ordinary TypeScript
via `ssr<T>()`; the fse-ssr Astro integration (`src/frontend/fse-ssr/`)
compiles those expressions to Tera in the built HTML, and the Rust backend
fills in the real values on every request:

```astro
---
import { ssr } from "fse-ssr";
import type { UserPage } from "../types/pages";

const { email, id, role, roles, error, t } = ssr<UserPage>();
---
<Header backlink="/users">{email}</Header>
<form action={`/users/${id}`} method="POST">
  <select name="role">
    {roles.map((r) => (
      <option value={r.value} selected={r.value === role}>{r.label}</option>
    ))}
  </select>
</form>
{error && <p class="banner">{error}</p>}
<p>{t.settings.delete_account.title}</p>
```

Declare each page's context shape in `src/types/pages.ts` (matching what the
service passes to `render_tpl`). `t` is typed from `locales/en.json`
(regenerated on every dev/build start), so translation typos fail
`astro check`.

Supported on SSR values: interpolation (text, attributes, template literals),
`.map()` (compiles to a `{% for %}` loop), `.length`, comparisons
(`===`, `!==`, `<`, …), `&&`/`||`/`!`/ternaries, and `?? fallback`
(compiles to Tera's `default` filter, which also makes optional context keys
safe). Everything else — `.filter()`, arithmetic, function calls — has no
server-side equivalent and fails the build; do it in the service or in
client code instead.

For client-side code, the backend serializes the page's full render context
into `<script type="application/json" id="__fse-props__">` (the placeholder
lives in `Layout.astro`). Read it with the same types — no extra request:

```ts
import { pageProps } from "fse-ssr/client";
const { rows } = pageProps<UsersPage>();
```

Note that this makes the entire render context visible to the client, so a
page's context must only contain data its viewer may see.

### Database migrations

Install sqlx cli if you don't have it:

```bash
cargo install sqlx-cli --no-default-features --features sqlite
```

Add a new migration:

```bash
sqlx migrate add migration_name
```

Execute all migrations that haven't been apllied:

```bash
sqlx migrate run
```

*Note: Before the webserver starts all migration are run to ensure that the database has everything in production.*

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