#!/usr/bin/env bash
# Local helper. Auto-bumps a per-OS rev when the staged hash changed.
# The deployer does not use this: it registers with --keep-author-revs
# so the author's rev is the version decision.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${FLEET_WORKER_DIST_DIR:-$ROOT/.tmp/vms/dist}"
exec python3 "$ROOT/scripts/runner-release.py" register "$DEST"
