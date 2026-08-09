#!/usr/bin/env bash
# Compose-level: provision-free job tree + named tool via workspace agent protocol (no LLM).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$ROOT/../../.." && pwd)"
cd "$ROOT"
OUT="${1:-}"
if [[ -z "$OUT" ]]; then
  echo "usage: $0 <output-log-path>" >&2
  exit 2
fi
mkdir -p "$(dirname "$OUT")"
# Bind-mount source must be a file; Docker otherwise creates a directory.
SSH_KEY_HOST_PATH="${SSH_KEY_HOST_PATH:-${REPO}/.tmp/ssh/id_ed25519}"
if [[ ! -f "${SSH_KEY_HOST_PATH}" ]]; then
  mkdir -p "$(dirname "${SSH_KEY_HOST_PATH}")"
  ssh-keygen -t ed25519 -f "${SSH_KEY_HOST_PATH}" -N "" -C "fleet-lab-dummy" >/dev/null
fi
export SSH_KEY_HOST_PATH
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
  alive=0
  for i in $(seq 1 50); do
    if dc exec -T workspace test -f /workspace/.fleet/agent.alive 2>/dev/null; then
      echo "workspace agent alive"
      alive=1
      break
    fi
    sleep 0.2
  done
  if [[ "${alive}" != "1" ]]; then
    echo "workspace agent never became alive" >&2
    exit 1
  fi

  JOB_ID="compose-int-$(date +%s)"
  CWD="/workspace/jobs/${JOB_ID}/repo"
  echo "=== seed local git repo for signed provision ==="
  dc exec -T workspace sh -c 'rm -rf /tmp/compose-int-src && mkdir -p /tmp/compose-int-src && cd /tmp/compose-int-src && git init -b main && git config user.email lab@example.test && git config user.name lab && echo compose-smoke > marker.txt && git add marker.txt && git commit -m init'

  echo "=== signed provision (registers TOOLS_BY_JOB) ==="
  ws_py prov provision <<'PY'
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
req = sign({
    "job_id": job_id,
    "action": "provision",
    "req_id": req_id,
    "repo_url": "/tmp/compose-int-src",
    "git_ref": "main",
    "work_dir": cwd,
    "commands": [],
    "smoke_commands": ["test -f marker.txt"],
    "env": {},
    "tools": {
        "read_marker": {"argv": ["cat", "marker.txt"], "description": "marker"},
        "list_dir": {"argv": ["ls", "-la"], "description": "ls"},
    },
})
(fleet / f"request-{req_id}.json").write_text(json.dumps(req))
res = fleet / f"result-{req_id}.json"
for _ in range(200):
    if res.is_file():
        d = json.loads(res.read_text())
        print(json.dumps(d, indent=2))
        assert d.get("ok") is True, d
        print("PROVISION_OK")
        break
    time.sleep(0.1)
else:
    raise SystemExit("timeout waiting for provision")
PY

  echo "=== unsigned named tool (fleet-wsexec path) ==="
  ws_py int1 known <<'PY'
import json, os, time
from pathlib import Path

job_id = os.environ["E2E_JOB_ID"]
cwd = os.environ["E2E_CWD"]
req_id = os.environ["E2E_REQ_ID"]
fleet = Path("/workspace/.fleet")
req = {"job_id": job_id, "action": "tool", "req_id": req_id, "tool": "read_marker", "cwd": cwd}
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
import json, os, time
from pathlib import Path

job_id = os.environ["E2E_JOB_ID"]
cwd = os.environ["E2E_CWD"]
req_id = os.environ["E2E_REQ_ID"]
fleet = Path("/workspace/.fleet")
req = {"job_id": job_id, "action": "tool", "req_id": req_id, "tool": "not_a_tool", "cwd": cwd}
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
import json, os, time
from pathlib import Path

job_id = os.environ["E2E_JOB_ID"]
req_id = os.environ["E2E_REQ_ID"]
fleet = Path("/workspace/.fleet")
evil = Path("/tmp/evil-tools")
evil.mkdir(exist_ok=True)
(evil / ".fleet-tools.json").write_text(json.dumps({"x": {"argv": ["echo", "no"]}}))
req = {"job_id": job_id, "action": "tool", "req_id": req_id, "tool": "x", "cwd": str(evil)}
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

  echo "=== hmac tool without provision must not use planted file ==="
  ws_py int-hmac planted <<'PY'
import hashlib, hmac, json, os, time
from pathlib import Path

def sign(req):
    secret = os.environ.get("FLEET_HANDSHAKE_SECRET") or ""
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    return body

req_id = os.environ["E2E_REQ_ID"]
fleet = Path("/workspace/.fleet")
planted_cwd = Path("/workspace/jobs/hmac-planted/repo")
planted_cwd.mkdir(parents=True, exist_ok=True)
(planted_cwd / ".fleet-tools.json").write_text(json.dumps({"pwn": {"argv": ["echo", "from-file"]}}))
req = sign({
    "job_id": "hmac-planted",
    "action": "tool",
    "req_id": req_id,
    "tool": "pwn",
    "cwd": str(planted_cwd),
})
(fleet / f"request-{req_id}.json").write_text(json.dumps(req))
res = fleet / f"result-{req_id}.json"
for _ in range(100):
    if res.is_file():
        d = json.loads(res.read_text())
        print(json.dumps(d, indent=2))
        assert d.get("ok") is False, d
        err = (d.get("error") or "").lower()
        assert "no registered tools" in err, d
        assert "from-file" not in (d.get("stdout") or "")
        print("HMAC_FILE_IGNORED")
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
