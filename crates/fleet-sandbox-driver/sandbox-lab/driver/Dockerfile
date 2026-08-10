# Multi-stage: musl-linked Rust driver + harness Alpine runtime (BASE_IMAGE).
# Preferred (pins the compose tag):
#   cd crates/fleet-sandbox-driver/sandbox-lab && docker compose build driver
# Or from repo root:
#   docker build -f crates/fleet-sandbox-driver/sandbox-lab/driver/Dockerfile \
#     --build-arg BASE_IMAGE=agent-fleet-opencode:alpine \
#     -t agent-fleet-sandbox-driver:local .

ARG BASE_IMAGE=agent-fleet-opencode:alpine
ARG RUST_VERSION=1.88

FROM rust:${RUST_VERSION}-bookworm AS builder
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends musl-tools musl-dev \
    && rm -rf /var/lib/apt/lists/*
# Map Docker arch → Rust musl target
RUN case "${TARGETARCH}" in \
      amd64) echo x86_64-unknown-linux-musl > /tmp/rust-target ;; \
      arm64) echo aarch64-unknown-linux-musl > /tmp/rust-target ;; \
      *) echo "unsupported TARGETARCH=${TARGETARCH}" >&2; exit 1 ;; \
    esac \
    && rustup target add "$(cat /tmp/rust-target)"
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN RUST_TARGET="$(cat /tmp/rust-target)" \
    && cargo build -p fleet-sandbox-driver --release --target "${RUST_TARGET}" --bin fleet-sandbox-driver \
    && strip "target/${RUST_TARGET}/release/fleet-sandbox-driver" \
    && cp "target/${RUST_TARGET}/release/fleet-sandbox-driver" /fleet-sandbox-driver

FROM ${BASE_IMAGE}
USER root
RUN apk add --no-cache openssh-client git \
    && mkdir -p /opt/fleet-sandbox-driver /workspace /home/fleet/.ssh /home/fleet/.config/opencode \
    && chown -R fleet:fleet /opt/fleet-sandbox-driver /workspace /home/fleet

COPY --from=builder /fleet-sandbox-driver /usr/local/bin/fleet-sandbox-driver
COPY crates/fleet-sandbox-driver/sandbox-lab/config/examples /opt/fleet-sandbox-driver/examples
RUN chmod 755 /usr/local/bin/fleet-sandbox-driver \
    && chown -R fleet:fleet /home/fleet

USER fleet
WORKDIR /workspace
ENV HOME=/home/fleet \
    PATH="/usr/local/bin:/home/fleet/.local/bin:${PATH}" \
    XDG_CONFIG_HOME=/home/fleet/.config \
    XDG_DATA_HOME=/home/fleet/.local/share \
    RUST_LOG=info

ENTRYPOINT ["fleet-sandbox-driver"]
CMD []
