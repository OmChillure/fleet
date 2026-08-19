#!/bin/sh
# Linux glibc aarch64 (Graviton, Raspberry Pi OS 64-bit)
# Fetched by install-worker.sh after platform detect. Do not run on the wrong OS.
set -eu
export FLEET_PLATFORM=linux-aarch64
ORIGIN="${FLEET_INSTALL_ORIGIN:-${FLEET_URL:-}}"
if [ -z "$ORIGIN" ]; then
  echo "worker install: set FLEET_URL to the control plane" >&2
  exit 1
fi
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$ORIGIN/install/worker/_unix_common.sh" | sh
elif command -v wget >/dev/null 2>&1; then
  wget -qO- "$ORIGIN/install/worker/_unix_common.sh" | sh
else
  echo "worker install: need curl or wget" >&2
  exit 1
fi
