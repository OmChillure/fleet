#!/usr/bin/env bash
# Harvest runner catalog status from test commands — not from agent IDs.
# Does not implement tests. Does not spawn writers. Does not skip identity.
set -euo pipefail
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"
BOARD="$REPO/.tmp/runner-catalog-board.json"
LOG="$REPO/.tmp/harvest-runner-catalog.log"
mkdir -p "$REPO/.tmp"

# Same files as the rest of the project (DATABASE_URL / JWT / PROFILE).
if [[ -f "$REPO/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  . "$REPO/.env"
  set +a
fi
if [[ -f "$REPO/.env.local" ]]; then
  set -a
  # shellcheck disable=SC1091
  . "$REPO/.env.local"
  set +a
fi
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-$REPO/.tmp/ms-playwright}"

TREE="$(bash "$REPO/scripts/tree-id.sh")"
TS="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
units_ok=0
e2e_status="fail"

exec > >(tee -a "$LOG") 2>&1

echo "==== $TS tree=$TREE ===="
echo "--- units ---"
if just test \
  && cargo test -p fleet-server --locked --test worker_hosts_http rename_host_patch_persists_rejects_empty_too_long \
  && cargo test -p fleet-server --locked --test worker_hosts_http request_update_marks_host; then
  units_ok=1
  echo "UNITS OK"
else
  echo "UNITS FAIL"
fi

echo "--- identity ---"
health="$(curl -fsS http://127.0.0.1:8080/health || true)"
ui="$(curl -fsS http://127.0.0.1:3030/fleet-git-sha.txt || true)"
sha="$(python3 -c 'import json,sys; print((json.loads(sys.argv[1] or "{}") or {}).get("git_sha") or "")' "$health" 2>/dev/null || true)"
echo "health.git_sha=$sha"
echo "ui.sha=$ui"
echo "tree=$TREE"

if [[ "$sha" != "$TREE" || "$ui" != "$TREE" ]]; then
  echo "E2E SKIP identity mismatch (rebuild this tree before e2e)"
  e2e_status="blocked-identity"
else
  echo "--- e2e ---"
  if python3 -m unittest discover -s tests/browser -p 'test_catalog_*.py' -v \
    && python3 -m unittest tests.browser.test_app_e2e -v; then
    e2e_status="pass"
    echo "E2E OK"
  else
    e2e_status="fail"
    echo "E2E FAIL"
  fi
fi

python3 - "$BOARD" "$TREE" "$TS" "$units_ok" "$e2e_status" <<'PY'
import json, sys
from pathlib import Path
board_path, tree, ts, units_ok, e2e_status = sys.argv[1:6]
p = Path(board_path)
data = json.loads(p.read_text()) if p.is_file() else {}
data["strategy"] = "harvest-from-suite"
data["tree_id"] = tree
data["updated"] = ts
tracks = data.setdefault("tracks", {})
tracks["units"] = {
    "status": "pass" if units_ok == "1" else "fail",
    "command": "just test + worker_hosts_http rename_host_patch_persists_rejects_empty_too_long + request_update_marks_host (DATABASE_URL from .env)",
}
tracks["e2e"] = {
    "status": e2e_status,
    "command": "python3 -m unittest discover -s tests/browser -p 'test_catalog_*.py' -v && python3 -m unittest tests.browser.test_app_e2e -v",
}
holes = {
    "catalog:unit-fleet-ui-helpers": "pass" if units_ok == "1" else "fail",
    "catalog:unit-route-nav-redirects": "pass" if units_ok == "1" else "fail",
    "catalog:unit-cloud-partners-helpers": "pass" if units_ok == "1" else "fail",
    "catalog:unit-patch-name": "pass" if units_ok == "1" else "fail",
    "catalog:unit-request-update": "pass" if units_ok == "1" else "fail",
    "catalog:e2e-rename": e2e_status if e2e_status == "pass" else e2e_status,
    "catalog:e2e-pending-apply-outdated-pills": e2e_status,
    "catalog:e2e-remove-confirm": e2e_status,
    "catalog:e2e-update-now": e2e_status,
    "catalog:e2e-poll": e2e_status,
    "catalog:e2e-specs": e2e_status,
    "catalog:e2e-software-line": e2e_status,
    "catalog:e2e-copy-buttons": e2e_status,
    "catalog:e2e-pair-done": e2e_status,
    "catalog:e2e-loopback-warning": e2e_status,
    "catalog:e2e-last-seen": e2e_status,
    "catalog:e2e-show-removed": e2e_status,
    "catalog:e2e-plane-sandboxed-toggle": e2e_status,
    "catalog:e2e-apply-toast-pending": e2e_status,
    "catalog:e2e-widgets-lock-pending": e2e_status,
    "catalog:e2e-partners-ack-persists": e2e_status,
    "catalog:e2e-cloud-coming-soon": e2e_status,
}
data["holes"] = holes
if units_ok == "1" and e2e_status == "pass":
    data["verdict"] = "SUITE-GREEN"
elif e2e_status == "blocked-identity":
    data["verdict"] = "CONTINUE"
else:
    data["verdict"] = "CONTINUE"
p.write_text(json.dumps(data, indent=2) + "\n")
PY

if [[ "$units_ok" != 1 || "$e2e_status" == "fail" ]]; then
  exit 1
fi
exit 0
