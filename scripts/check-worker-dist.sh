#!/usr/bin/env bash
# Fail when staged shipped-OS binaries do not match the registered
# version × hash table in runner-packages.toml [release].
# fleet-dev still starts (it calls this with || true). Deploy does not.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${1:-${FLEET_WORKER_DIST_DIR:-$ROOT/.tmp/vms/dist}}"
exec python3 "$ROOT/scripts/runner-release.py" check "$DEST"
