FROM oven/bun:1.3.8-alpine AS frontend-builder

WORKDIR /app/frontend

COPY src/frontend/package.json src/frontend/bun.lock ./

RUN bun install

COPY src/frontend .

RUN bun run build

FROM rust:1.93.0-slim AS backend-builder

WORKDIR /app

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./

# dummy build for caching dependencies
RUN mkdir -p src/ \
    && echo "fn main() {}" > src/main.rs \
    && cargo build --release

RUN rm src/main.rs

COPY src ./src
COPY .sqlx ./.sqlx
COPY migrations ./migrations

ENV SQLX_OFFLINE=true

COPY --from=frontend-builder /app/frontend/dist ./src/frontend/dist

RUN cargo build --release

FROM debian:trixie-slim AS runtime

WORKDIR /app

RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl-dev \
    openssl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=backend-builder /app/target/release/starter ./starter

EXPOSE 8080

CMD ["./starter"]
