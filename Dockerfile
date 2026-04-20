# Image for testing
FROM oven/bun:1.3.8-alpine AS frontend-builder
WORKDIR /app/frontend
COPY starter/src/frontend/package.json starter/src/frontend/bun.lock ./
RUN bun install
COPY starter/src/frontend .
RUN bun run build

FROM rust:1.93.0-slim AS backend-builder
WORKDIR /app
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY framework ../framework
COPY starter .

COPY --from=frontend-builder /app/frontend/dist ./src/frontend/dist

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
COPY --from=backend-builder /app/migrations ./migrations
EXPOSE 8080
CMD ["./starter"]
