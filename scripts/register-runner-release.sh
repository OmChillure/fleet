#!/usr/bin/env bash
# After bumping a bin's [versions]/[verify]/[release.BIN].version and
# restaging, write that bin's per-OS sha256s + parent into the toml.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${FLEET_WORKER_DIST_DIR:-$ROOT/.tmp/vms/dist}"
exec python3 "$ROOT/scripts/runner-release.py" register "$DEST"
