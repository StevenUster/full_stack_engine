FROM oven/bun:1.3.8-alpine AS theme-builder
WORKDIR /app/theme
COPY theme/package.json theme/bun.lock ./
RUN bun install
COPY theme .
COPY locales ../locales
RUN bun run build

FROM rust:1.93.0-slim AS backend-builder
WORKDIR /app
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY . .
COPY --from=theme-builder /app/theme/dist ./theme/dist

ENV SQLX_OFFLINE=true
RUN cargo build --release

FROM debian:trixie-slim AS runtime
WORKDIR /app
RUN mkdir -p data
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl-dev \
    openssl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=backend-builder /app/target/release/starter ./starter
# Migrations, locales and both themes are embedded in the binary — nothing
# else to copy.
EXPOSE 8080
CMD ["./starter"]
