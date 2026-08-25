# fleet-server (API + auth + integrations + orchestrator)
# Build: docker build --build-arg FLEET_GIT_SHA=$(git rev-parse HEAD) -t agent-fleet-api:local .
# Run with DATABASE_URL pointing at Postgres.

FROM rust:1.88-bookworm AS builder
WORKDIR /app

# Cache deps
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY scripts/install-worker ./scripts/install-worker
# Bake HEAD into the binary (option_env!). Empty ARG is a failed build, not a
# null /health that live tests could then "fix" with kubectl set env.
ARG FLEET_GIT_SHA
RUN test -n "${FLEET_GIT_SHA}" || (echo "ERROR: --build-arg FLEET_GIT_SHA=\$(git rev-parse HEAD) is required" && exit 1)
ENV FLEET_GIT_SHA=${FLEET_GIT_SHA}
RUN cargo build --release -p fleet-server --locked \
    && cargo build --release -p fleet-host --locked \
    && cargo build --release -p fleet-driver --locked --bins

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 10001 fleet

WORKDIR /app
COPY --from=builder /app/target/release/fleet-server /usr/local/bin/fleet-server
COPY --from=builder /app/target/release/fleet-host /usr/local/bin/fleet-host
COPY --from=builder /app/target/release/fleet-driver /usr/local/bin/fleet-driver
COPY --from=builder /app/target/release/mock-acp-agent /usr/local/bin/mock-acp-agent

# Session work dirs for process sandbox
RUN mkdir -p /data && chown fleet:fleet /data
USER fleet
ENV DATA_DIR=/data
ENV HOST=0.0.0.0
ENV PORT=8080
# Label only — do not ENV FLEET_GIT_SHA here. Health reads the baked binary.
ARG FLEET_GIT_SHA
RUN test -n "${FLEET_GIT_SHA}"
LABEL fleet.git_sha=${FLEET_GIT_SHA}
EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=3s --start-period=20s --retries=5 \
  CMD curl -sf http://127.0.0.1:8080/health || exit 1

CMD ["fleet-server"]
