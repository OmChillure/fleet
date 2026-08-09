# fleet-server (API + auth + integrations + orchestrator)
# Build: docker build -t fleet-server .
# Run with DATABASE_URL pointing at Postgres.

FROM rust:1.88-bookworm AS builder
WORKDIR /app

# Cache deps
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release -p fleet-server --locked \
    && cargo build --release -p fleet-orchestrator --bin fleet-worker --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 10001 fleet

WORKDIR /app
COPY --from=builder /app/target/release/fleet-server /usr/local/bin/fleet-server
COPY --from=builder /app/target/release/fleet-worker /usr/local/bin/fleet-worker

# Session work dirs for process sandbox
RUN mkdir -p /data && chown fleet:fleet /data
USER fleet
ENV DATA_DIR=/data
ENV HOST=0.0.0.0
ENV PORT=8080
EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=3s --start-period=20s --retries=5 \
  CMD curl -sf http://127.0.0.1:8080/health || exit 1

CMD ["fleet-server"]
