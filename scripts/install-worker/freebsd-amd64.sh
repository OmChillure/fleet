#!/bin/sh
# FreeBSD amd64
# Fetched by install-worker.sh after platform detect. Do not run on the wrong OS.
set -eu
export FLEET_PLATFORM=freebsd-amd64
ORIGIN="${FLEET_INSTALL_ORIGIN:-${FLEET_URL:-}}"
if [ -z "$ORIGIN" ]; then
  echo "fleet-host install: set FLEET_URL to the control plane" >&2
  exit 1
fi
if command -v curl >/dev/null 2>&1; then
  curl -fsSL "$ORIGIN/install/worker/_unix_common.sh" | sh
elif command -v wget >/dev/null 2>&1; then
  wget -qO- "$ORIGIN/install/worker/_unix_common.sh" | sh
elif command -v fetch >/dev/null 2>&1; then
  # Stock FreeBSD has fetch(1), not curl/wget. fetch -o dest url; dest=- is stdout.
  fetch -o - "$ORIGIN/install/worker/_unix_common.sh" | sh
else
  echo "fleet-host install: need curl, wget, or fetch" >&2
  exit 1
fi
