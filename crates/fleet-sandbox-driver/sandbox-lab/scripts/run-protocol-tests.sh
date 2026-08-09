#!/usr/bin/env bash
# Thin wrapper: prefer scripts/run-full-suite.sh (same cargo + protocol gates).
# Kept for README / habit; optional log path mirrors the old interface.
set -euo pipefail
LAB="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(cd "${LAB}/../../.." && pwd)"
cd "${REPO}"
OUT="${1:-}"

echo "prefer run-full-suite.sh (this wrapper runs cargo -p fleet-sandbox-driver + workspace_agent protocol only)" >&2

run() {
  echo "=== cargo test -p fleet-sandbox-driver ==="
  cargo test -p fleet-sandbox-driver --offline --locked -- --nocapture
  echo
  echo "=== workspace_agent protocol (unittest) ==="
  python3 "${LAB}/workspace/tests/test_workspace_agent_protocol.py" -v
  echo
  echo "ALL PROTOCOL/UNIT GATES PASSED"
}

if [[ -n "${OUT}" ]]; then
  mkdir -p "$(dirname "${OUT}")"
  run 2>&1 | tee "${OUT}"
else
  run
fi
