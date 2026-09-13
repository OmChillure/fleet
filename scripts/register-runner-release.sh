#!/usr/bin/env bash
# After bumping [versions]/[verify]/[release].version and restaging the
# shipped OS packages, write their sha256s + parent into runner-packages.toml.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${FLEET_WORKER_DIST_DIR:-$ROOT/.tmp/vms/dist}"
exec python3 "$ROOT/scripts/runner-release.py" register "$DEST"
