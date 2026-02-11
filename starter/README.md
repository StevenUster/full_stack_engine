# Starter

## Deployment

Prepare sqlx queries:

```bash
cargo sqlx prepare
```

Build the image

```bash
VERSION=$(grep "^version =" Cargo.toml | cut -d '"' -f 2)
podman build -t ghcr.io/stevenuster/starter:latest -t ghcr.io/stevenuster/starter:$VERSION .
```

Push the image to ghcr.io:

```bash
VERSION=$(grep "^version =" Cargo.toml | cut -d '"' -f 2)
podman push ghcr.io/stevenuster/starter:latest
podman push ghcr.io/stevenuster/starter:$VERSION
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