# Pi coding agent (https://pi.dev) + pi-acp adapter (ACP over stdio).
# Spawns: pi-acp  (bridges ACP ↔ pi --mode rpc)
# Local models: ~/.pi/agent/models.json (written by fleet-driver from job.provider)
#
# Build (from crates/fleet-sandbox-driver/agents or via ./build.sh):
#   docker build -t agent-fleet-base:alpine -f base/Dockerfile base
#   docker build -t agent-fleet-pi:alpine -f pi/Dockerfile pi
ARG BASE_IMAGE=agent-fleet-base:alpine
FROM ${BASE_IMAGE}

ARG PI_VERSION=0.83.0
ARG PI_ACP_VERSION=0.0.33

USER root
ENV NPM_CONFIG_PREFIX=/usr/local
# pi-acp wants Node 22+; Alpine 3.21 nodejs is typically fine.
RUN npm install -g \
      "@earendil-works/pi-coding-agent@${PI_VERSION}" \
      "pi-acp@${PI_ACP_VERSION}" \
    && pi --version \
    && command -v pi-acp \
    && (pi-acp --help >/dev/null 2>&1 || true) \
    && npm cache clean --force \
    && mkdir -p /home/fleet/.pi/agent /home/fleet/.pi/pi-acp \
    && chown -R fleet:fleet /home/fleet

USER fleet
ENV HOME=/home/fleet \
    PATH="/usr/local/bin:/home/fleet/.local/bin:${PATH}" \
    XDG_DATA_HOME=/home/fleet/.local/share \
    XDG_CONFIG_HOME=/home/fleet/.config

WORKDIR /workspace

# Host/fleet-driver controls this process over Docker stdio, e.g.:
#   docker run --rm -i -v "$PWD:/workspace" agent-fleet-pi:alpine
ENTRYPOINT ["/sbin/tini", "--", "pi-acp"]
CMD []
