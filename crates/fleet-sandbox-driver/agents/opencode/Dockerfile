# OpenCode ACP agent (Alpine / musl).
# Spawns: opencode acp
# Auth:   provider keys via env or mounted config (~/.local/share/opencode, etc.)
#
# OpenCode ships musl optionalDependencies (opencode-linux-*-musl) — preferred on Alpine.
ARG BASE_IMAGE=agent-fleet-base:alpine
FROM ${BASE_IMAGE}

ARG OPENCODE_VERSION=1.18.9

USER root
ENV NPM_CONFIG_PREFIX=/usr/local
# Force musl optional package selection via Alpine's musl platform.
RUN npm install -g "opencode-ai@${OPENCODE_VERSION}" \
    && opencode --version \
    && npm cache clean --force \
    # npm/root may create ~/.local owned by root without group write — opencode logs there.
    && mkdir -p /home/fleet/.local/share/opencode /home/fleet/.config/opencode \
    && chown -R fleet:fleet /home/fleet

USER fleet
ENV HOME=/home/fleet \
    PATH="/usr/local/bin:/home/fleet/.local/bin:${PATH}" \
    XDG_DATA_HOME=/home/fleet/.local/share \
    XDG_CONFIG_HOME=/home/fleet/.config

WORKDIR /workspace

# IMPORTANT: do not put -m before `acp` — model is set via ACP set_config_option only.
# Host driver controls this process over Docker stdio, e.g.:
#   docker run --rm -i -v "$PWD:/workspace" agent-fleet-opencode:alpine
ENTRYPOINT ["/sbin/tini", "--", "opencode"]
CMD ["acp"]
