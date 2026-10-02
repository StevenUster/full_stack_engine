FROM rust:1.93.0-slim AS builder
WORKDIR /app
COPY . .
# Queries are checked against the committed .sqlx/ cache (`fse migrate`
# refreshes it); no database is needed to build.
ENV SQLX_OFFLINE=true
RUN cargo build --release

FROM debian:trixie-slim AS runtime
WORKDIR /app
RUN mkdir -p data && apt-get update && apt-get install -y ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/__NAME__ ./app
# Migrations, locales and themes are embedded in the binary.
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD ["./app", "--healthcheck"]
CMD ["./app"]
