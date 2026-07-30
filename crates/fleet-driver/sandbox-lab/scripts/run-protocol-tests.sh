#!/usr/bin/env bash
# Headless protocol + cargo tests for sandbox block (no LLM required).
set -euo pipefail
# scripts/ → sandbox-lab/ → fleet-driver/ → crates/ → repo root
LAB="$(cd "$(dirname "$0")/.." && pwd)"
ROOT="$(cd "$LAB/../../.." && pwd)"
cd "$ROOT"
OUT="${1:-}"
if [[ -z "$OUT" ]]; then
  echo "usage: $0 <output-log-path>" >&2
  exit 2
fi
mkdir -p "$(dirname "$OUT")"
{
  echo "=== cargo test -p fleet-driver ==="
  cargo test -p fleet-driver -- --nocapture
  echo
  echo "=== workspace_agent protocol (unittest) ==="
  python3 "$LAB/workspace/tests/test_workspace_agent_protocol.py" -v
  echo
  echo "=== seed-from-env defaults check ==="
  python3 - <<'PY'
from pathlib import Path
p = Path("crates/fleet-driver/sandbox-lab/scripts/seed-from-env.sh")
src = p.read_text()
assert '"auto_approve": False' in src or '"auto_approve": false' in src
assert "permission_allow" in src
assert '"work_dir": "/workspace/repo"' not in src
print("seed-from-env.sh: default-deny + no hardcoded /workspace/repo OK")
PY
  echo
  echo "ALL PROTOCOL/UNIT GATES PASSED"
} 2>&1 | tee "$OUT"
