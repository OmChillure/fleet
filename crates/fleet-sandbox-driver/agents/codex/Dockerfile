# Codex ACP agent (Alpine).
# Spawns: codex-acp  (stdio ACP)
# Auth:   OPENAI_API_KEY (or Codex login volume)
#
# Build (from crates/fleet-sandbox-driver/agents or via ./build.sh):
#   docker build -t agent-fleet-base:alpine -f base/Dockerfile base
#   docker build -t agent-fleet-codex:alpine -f codex/Dockerfile codex
ARG BASE_IMAGE=agent-fleet-base:alpine
FROM ${BASE_IMAGE}

ARG CODEX_ACP_VERSION=1.1.7

USER root
# Global install under /usr/local so PATH works for fleet user.
ENV NPM_CONFIG_PREFIX=/usr/local
RUN npm install -g "@agentclientprotocol/codex-acp@${CODEX_ACP_VERSION}" \
    && codex-acp --version \
    && (command -v codex >/dev/null && codex --version || true) \
    && npm cache clean --force \
    && mkdir -p /home/fleet/.codex \
    && chown -R fleet:fleet /home/fleet

USER fleet
ENV HOME=/home/fleet \
    PATH="/usr/local/bin:/home/fleet/.local/bin:/home/fleet/.grok/bin:${PATH}"

WORKDIR /workspace

# ACP over stdio — no extra flags; driver injects --model when needed.
ENTRYPOINT ["/sbin/tini", "--", "codex-acp"]
CMD []
