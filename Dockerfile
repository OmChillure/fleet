# syntax=docker/dockerfile:1
# fleet-server (API + auth + integrations + orchestrator)
# Build: docker build --build-arg FLEET_GIT_SHA=$(./scripts/tree-id.sh) -t agent-fleet-api:local .
# Run with DATABASE_URL pointing at Postgres.
#
# FLEET_GIT_SHA is the identity bake (option_env!), not the dep-compile cache
# key. Fetch + warm cargo happen before ARG. The sha-consuming cargo is after.

FROM rust:1.88-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY runner-packages.toml ./
COPY crates ./crates
COPY scripts/install-worker ./scripts/install-worker

# Warm registry + release artifacts. No FLEET_GIT_SHA — a dirty tree-id must
# not re-download crates.io or rebuild kube/sqlx/zenoh.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=fleet-api-target,target=/app/target \
    cargo fetch --locked \
    && cargo build --release --locked \
        -p fleet-server -p fleet-host -p fleet-driver --bins

# Bake HEAD into fleet-server / fleet-host (option_env!). Empty ARG is a
# failed build, not a null /health that live tests could then "fix" with
# kubectl set env.
ARG FLEET_GIT_SHA
RUN test -n "${FLEET_GIT_SHA}" || (echo "ERROR: --build-arg FLEET_GIT_SHA=\$(git rev-parse HEAD) is required" && exit 1)
ENV FLEET_GIT_SHA=${FLEET_GIT_SHA}
# Cache mount is not in the image — copy bins out in this RUN.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=fleet-api-target,target=/app/target \
    cargo build --release --locked -p fleet-server -p fleet-host -p fleet-driver --bins \
    && mkdir -p /out \
    && cp target/release/fleet-server \
          target/release/fleet-host \
          target/release/fleet-driver \
          target/release/mock-acp-agent \
          /out/

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 10001 fleet

WORKDIR /app
COPY --from=builder /out/fleet-server /usr/local/bin/fleet-server
COPY --from=builder /out/fleet-host /usr/local/bin/fleet-host
COPY --from=builder /out/fleet-driver /usr/local/bin/fleet-driver
COPY --from=builder /out/mock-acp-agent /usr/local/bin/mock-acp-agent

# Session work dirs for process sandbox
RUN mkdir -p /data && chown fleet:fleet /data
USER fleet
ENV DATA_DIR=/data
ENV HOST=0.0.0.0
ENV PORT=8080
# Label only — do not inject the sha as a runtime env. Health reads the baked binary.
ARG FLEET_GIT_SHA
RUN test -n "${FLEET_GIT_SHA}"
LABEL fleet.git_sha=${FLEET_GIT_SHA}
EXPOSE 8080

HEALTHCHECK --interval=10s --timeout=3s --start-period=20s --retries=5 \
  CMD curl -sf http://127.0.0.1:8080/health || exit 1

CMD ["fleet-server"]
