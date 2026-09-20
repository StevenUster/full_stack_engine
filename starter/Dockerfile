FROM oven/bun:1.3.8-alpine AS theme-builder
WORKDIR /app/theme
COPY theme/package.json theme/bun.lock ./
RUN bun install
COPY theme .
COPY locales ../locales
RUN bun run build

FROM rust:1.93.0-slim AS backend-builder
WORKDIR /app
# No pkg-config/libssl-dev: TLS is rustls throughout (sqlx uses `tls-none`
# because SQLite is a local file, lettre and reqwest use rustls), so nothing in
# the tree links OpenSSL. `cargo deny` enforces that it stays that way.

COPY . .
COPY --from=theme-builder /app/theme/dist ./theme/dist

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
# Migrations, locales and both themes are embedded in the binary — nothing
# else to copy.
EXPOSE 8080
# The framework serves /healthz (liveness, touches nothing) and /readyz
# (readiness, pings the pool). Liveness is the right one here: a failing
# database should not get the container restarted, which fixes nothing.
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["./starter", "--healthcheck"]
CMD ["./starter"]
