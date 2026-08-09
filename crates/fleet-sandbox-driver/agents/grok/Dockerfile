# Grok Build ACP agent (Alpine + gcompat).
# Spawns: grok agent stdio
# Auth:   GROK_DEPLOYMENT_KEY or mounted ~/.grok/auth.json from `grok login`
#
# Official install downloads a linux-{x86_64,aarch64} binary (glibc). On Alpine
# we rely on gcompat from the base image. If the binary refuses to run, switch
# this image to debian:bookworm-slim as a fallback.
ARG BASE_IMAGE=agent-fleet-base:alpine
FROM ${BASE_IMAGE}

ARG GROK_VERSION=
ARG GROK_CHANNEL=stable

USER root
# Installer expects a writable HOME and bash; install as root then rehome.
ENV HOME=/root \
    GROK_BIN_DIR=/usr/local/bin \
    GROK_CHANNEL=${GROK_CHANNEL}

RUN set -eux; \
    if [ -n "${GROK_VERSION}" ]; then \
      curl -fsSL https://x.ai/cli/install.sh | bash -s "${GROK_VERSION}"; \
    else \
      curl -fsSL https://x.ai/cli/install.sh | bash; \
    fi; \
    # install.sh may place binary in GROK_BIN_DIR or ~/.grok/bin
    if [ -x /usr/local/bin/grok ]; then \
      true; \
    elif [ -x /root/.grok/bin/grok ]; then \
      install -m 0755 /root/.grok/bin/grok /usr/local/bin/grok; \
    else \
      echo "grok binary not found after install" >&2; \
      find /root /usr/local -name 'grok' -type f 2>/dev/null || true; \
      exit 1; \
    fi; \
    # install.sh often leaves a symlink into /root/.grok/downloads (mode 0700).
    # The fleet user (uid 1000) cannot read that path — copy the real binary.
    if [ -L /usr/local/bin/grok ]; then \
      src="$(readlink -f /usr/local/bin/grok)"; \
      install -m 0755 "${src}" /usr/local/bin/grok; \
    fi; \
    chmod 755 /usr/local/bin/grok; \
    # Smoke-test under gcompat; fail the build early if musl is incompatible.
    /usr/local/bin/grok --version

# Fleet user needs a writable ~/.grok for auth/cache.
RUN mkdir -p /home/fleet/.grok \
    && chown -R fleet:fleet /home/fleet/.grok

USER fleet
ENV HOME=/home/fleet \
    PATH="/usr/local/bin:/home/fleet/.local/bin:/home/fleet/.grok/bin:${PATH}"

WORKDIR /workspace

ENTRYPOINT ["/sbin/tini", "--", "grok"]
CMD ["agent", "stdio"]
