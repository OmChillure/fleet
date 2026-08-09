#!/usr/bin/env bash
# Compose-level: provision-free job tree + named tool via workspace agent protocol (no LLM).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
OUT="${1:-}"
if [[ -z "$OUT" ]]; then
  echo "usage: $0 <output-log-path>" >&2
  exit 2
fi
mkdir -p "$(dirname "$OUT")"
export COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-fleet-compose-int-$$}"
dc() { docker compose -p "$COMPOSE_PROJECT_NAME" "$@"; }
trap 'dc down -v --remove-orphans >/dev/null 2>&1 || true' EXIT

# stdin → python3 in workspace (no nested bash quoting)
ws_py() {
  dc exec -T \
    -e "E2E_JOB_ID=${JOB_ID}" \
    -e "E2E_CWD=${CWD}" \
    -e "E2E_REQ_ID=${1}" \
    -e "E2E_CASE=${2}" \
    workspace python3 -
}

{
  echo "=== docker compose up redis workspace (project=$COMPOSE_PROJECT_NAME) ==="
  if ! dc up -d redis workspace; then
    echo "compose unavailable"
    exit 10
  fi
  dc ps
  for i in $(seq 1 50); do
    if dc exec -T workspace test -f /workspace/.fleet/agent.alive 2>/dev/null; then
      echo "workspace agent alive"
      break
    fi
    sleep 0.2
  done

  JOB_ID="compose-int-$(date +%s)"
  CWD="/workspace/jobs/${JOB_ID}/repo"
  echo "=== create job tree + tools at $CWD ==="
  dc exec -T workspace mkdir -p "$CWD"
  dc exec -T workspace sh -c "echo compose-smoke > '$CWD/marker.txt'"
  dc exec -T workspace python3 -c "import json,pathlib; p=pathlib.Path('$CWD')/'.fleet-tools.json'; p.write_text(json.dumps({'read_marker':{'argv':['cat','marker.txt']},'list_dir':{'argv':['ls','-la']}}))"
  dc exec -T workspace chmod -R a+rwX "/workspace/jobs/$JOB_ID" || true

  echo "=== named tool request (known) ==="
  ws_py int1 known <<'PY'
import hashlib, hmac, json, os, time
from pathlib import Path

def sign(req):
    secret = os.environ.get("FLEET_HANDSHAKE_SECRET") or ""
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    return body

job_id = os.environ["E2E_JOB_ID"]
cwd = os.environ["E2E_CWD"]
req_id = os.environ["E2E_REQ_ID"]
fleet = Path("/workspace/.fleet")
req = sign({"job_id": job_id, "action": "tool", "req_id": req_id, "tool": "read_marker", "cwd": cwd})
(fleet / f"request-{req_id}.json").write_text(json.dumps(req))
res = fleet / f"result-{req_id}.json"
for _ in range(100):
    if res.is_file():
        d = json.loads(res.read_text())
        print(json.dumps(d, indent=2))
        assert d.get("ok") is True, d
        assert "compose-smoke" in (d.get("stdout") or ""), d
        print("KNOWN_TOOL_OK")
        break
    time.sleep(0.1)
else:
    raise SystemExit("timeout waiting for tool result")
PY

  echo "=== unknown tool rejected ==="
  ws_py int-bad unknown <<'PY'
import hashlib, hmac, json, os, time
from pathlib import Path

def sign(req):
    secret = os.environ.get("FLEET_HANDSHAKE_SECRET") or ""
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    return body

job_id = os.environ["E2E_JOB_ID"]
cwd = os.environ["E2E_CWD"]
req_id = os.environ["E2E_REQ_ID"]
fleet = Path("/workspace/.fleet")
req = sign({"job_id": job_id, "action": "tool", "req_id": req_id, "tool": "not_a_tool", "cwd": cwd})
(fleet / f"request-{req_id}.json").write_text(json.dumps(req))
res = fleet / f"result-{req_id}.json"
for _ in range(100):
    if res.is_file():
        d = json.loads(res.read_text())
        print(json.dumps(d, indent=2))
        assert d.get("ok") is False, d
        err = (d.get("error") or d.get("stderr") or "")
        assert "not allowlisted" in err or "not_a_tool" in err, d
        print("UNKNOWN_TOOL_REJECTED")
        break
    time.sleep(0.1)
else:
    raise SystemExit("timeout")
PY

  echo "=== cwd outside jobs rejected ==="
  ws_py int-out outside <<'PY'
import hashlib, hmac, json, os, time
from pathlib import Path

def sign(req):
    secret = os.environ.get("FLEET_HANDSHAKE_SECRET") or ""
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    return body

job_id = os.environ["E2E_JOB_ID"]
req_id = os.environ["E2E_REQ_ID"]
fleet = Path("/workspace/.fleet")
evil = Path("/tmp/evil-tools")
evil.mkdir(exist_ok=True)
(evil / ".fleet-tools.json").write_text(json.dumps({"x": {"argv": ["echo", "no"]}}))
req = sign({"job_id": job_id, "action": "tool", "req_id": req_id, "tool": "x", "cwd": str(evil)})
(fleet / f"request-{req_id}.json").write_text(json.dumps(req))
res = fleet / f"result-{req_id}.json"
for _ in range(100):
    if res.is_file():
        d = json.loads(res.read_text())
        print(json.dumps(d, indent=2))
        assert d.get("ok") is False, d
        assert "outside job isolation" in (d.get("error") or ""), d
        print("OUTSIDE_CWD_REJECTED")
        break
    time.sleep(0.1)
else:
    raise SystemExit("timeout")
PY

  echo "=== assert job workdir + tools file ==="
  dc exec -T workspace test -f "$CWD/.fleet-tools.json"
  dc exec -T workspace cat "$CWD/.fleet-tools.json"
  echo "COMPOSE_TOOL_INTEGRATION_OK job_id=$JOB_ID cwd=$CWD"
} 2>&1 | tee "$OUT"
status=${PIPESTATUS[0]}
exit $status
