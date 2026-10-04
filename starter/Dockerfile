# 1.3.14, not older: there is no Node in this image, so Astro builds on the
# Bun runtime, and Bun 1.3.8 crashes loading react-dom/server.bun.js
# ("Expected CommonJS module to have a function wrapper").
FROM oven/bun:1.3.14-alpine AS theme-builder
WORKDIR /app
# Every npm theme in themes/ is built: the binary embeds all of them, and
# fse.toml only picks the active one. Installs first, since a child theme
# builds from its sibling parent's sources. node_modules is dropped
# afterwards so only the built dist/ folders reach the next stage.
COPY locales locales
COPY themes themes
RUN set -e; \
    for t in themes/*/; do if [ -f "$t/package.json" ]; then (cd "$t" && bun install --frozen-lockfile); fi; done; \
    for t in themes/*/; do if [ -f "$t/package.json" ]; then (cd "$t" && bun run build); fi; done; \
    rm -rf themes/*/node_modules

FROM rust:1.93.0-slim AS backend-builder
WORKDIR /app
# No pkg-config/libssl-dev: TLS is rustls throughout (sqlx uses `tls-none`
# because SQLite is a local file, lettre and reqwest use rustls), so nothing in
# the tree links OpenSSL. `cargo deny` enforces that it stays that way.

COPY . .
COPY --from=theme-builder /app/themes ./themes

ENV SQLX_OFFLINE=true
RUN cargo build --release

FROM debian:trixie-slim AS runtime
WORKDIR /app
RUN mkdir -p data
# `ca-certificates` only. rustls ships the webpki root store for SMTP, but
# reqwest's platform verifier still reads the system store, so the bundle stays.
RUN apt-get update && apt-get install -y \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=backend-builder /app/target/release/starter ./starter
# Migrations, locales and every theme are embedded in the binary — nothing
# else to copy.
EXPOSE 8080
# The framework serves /healthz (liveness, touches nothing) and /readyz
# (readiness, pings the pool). Liveness is the right one here: a failing
# database should not get the container restarted, which fixes nothing.
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["./starter", "--healthcheck"]
CMD ["./starter"]
