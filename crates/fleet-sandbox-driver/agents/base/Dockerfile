# Agent Fleet — shared Alpine base for ACP harness images.
# Provides Node/npm (for npm-distributed agents), git, and glibc compat (gcompat)
# so glibc-linked agent binaries (codex, grok) can run on musl Alpine.
FROM alpine:3.21

ARG NODE_PKG=nodejs
ARG NPM_PKG=npm

LABEL org.opencontainers.image.title="agent-fleet-base" \
      org.opencontainers.image.description="Alpine base for Agent Fleet ACP agent images" \
      org.opencontainers.image.source="https://github.com/xai-org/agent-fleet"

RUN apk add --no-cache \
      bash \
      ca-certificates \
      curl \
      git \
      gcompat \
      libstdc++ \
      libgcc \
      ${NODE_PKG} \
      ${NPM_PKG} \
      python3 \
      tini \
    && update-ca-certificates \
    && node --version \
    && npm --version \
    && git --version

# Non-root runtime user; mount project at /workspace.
RUN addgroup -g 1000 fleet \
    && adduser -D -u 1000 -G fleet -h /home/fleet -s /bin/bash fleet \
    && mkdir -p /workspace \
    && chown fleet:fleet /workspace

ENV HOME=/home/fleet \
    PATH="/home/fleet/.local/bin:/home/fleet/.grok/bin:/usr/local/bin:${PATH}" \
    NPM_CONFIG_PREFIX=/home/fleet/.local \
    npm_config_update_notifier=false

WORKDIR /workspace

USER fleet

# Default entry is a shell; agent images override CMD/ENTRYPOINT.
ENTRYPOINT ["/sbin/tini", "--"]
CMD ["bash"]
