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
# Dedicated project/volume for this integration run (do not share with invoices tasks)
export COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-fleet-compose-int-$$}"
dc() { docker compose -p "$COMPOSE_PROJECT_NAME" "$@"; }
trap 'dc down -v --remove-orphans >/dev/null 2>&1 || true' EXIT
{
  echo "=== docker compose up redis workspace (project=$COMPOSE_PROJECT_NAME) ==="
  if ! dc up -d redis workspace; then
    echo "compose unavailable"
    exit 10
  fi
  dc ps
  # Wait for agent.alive
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
  dc exec -T workspace bash -lc "
    set -e
    mkdir -p '$CWD'
    echo compose-smoke > '$CWD/marker.txt'
    cat > '$CWD/.fleet-tools.json' <<'JSON'
{
  \"read_marker\": {\"argv\": [\"cat\", \"marker.txt\"], \"description\": \"read marker\"},
  \"list_dir\": {\"argv\": [\"ls\", \"-la\"], \"description\": \"list\"}
}
JSON
    chmod -R a+rwX /workspace/jobs/$JOB_ID || true
  "

  REQ_ID="int1"
  echo "=== named tool request (known) ==="
  dc exec -T workspace bash -lc "
    python3 - <<'PY'
import hashlib, hmac, json, os, time
from pathlib import Path

def sign(req):
    secret = os.environ.get("FLEET_HANDSHAKE_SECRET") or ""
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    return body

fleet = Path('/workspace/.fleet')
req = sign({
  'job_id': '$JOB_ID',
  'action': 'tool',
  'req_id': '$REQ_ID',
  'tool': 'read_marker',
  'cwd': '$CWD',
})
(fleet / 'request-$REQ_ID.json').write_text(json.dumps(req))
res = fleet / 'result-$REQ_ID.json'
for _ in range(100):
    if res.is_file():
        d = json.loads(res.read_text())
        print(json.dumps(d, indent=2))
        assert d.get('ok') is True, d
        assert 'compose-smoke' in (d.get('stdout') or ''), d
        print('KNOWN_TOOL_OK')
        break
    time.sleep(0.1)
else:
    raise SystemExit('timeout waiting for tool result')
PY
  "

  echo "=== unknown tool rejected ==="
  REQ_ID2="int-bad"
  dc exec -T workspace bash -lc "
    python3 - <<'PY'
import hashlib, hmac, json, os, time
from pathlib import Path

def sign(req):
    secret = os.environ.get("FLEET_HANDSHAKE_SECRET") or ""
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    return body

fleet = Path('/workspace/.fleet')
req = sign({
  'job_id': '$JOB_ID',
  'action': 'tool',
  'req_id': '$REQ_ID2',
  'tool': 'not_a_tool',
  'cwd': '$CWD',
})
(fleet / 'request-$REQ_ID2.json').write_text(json.dumps(req))
res = fleet / 'result-$REQ_ID2.json'
for _ in range(100):
    if res.is_file():
        d = json.loads(res.read_text())
        print(json.dumps(d, indent=2))
        assert d.get('ok') is False, d
        err = (d.get('error') or d.get('stderr') or '')
        assert 'not allowlisted' in err or 'not_a_tool' in err, d
        print('UNKNOWN_TOOL_REJECTED')
        break
    time.sleep(0.1)
else:
    raise SystemExit('timeout')
PY
  "

  echo "=== cwd outside jobs rejected ==="
  REQ_ID3="int-out"
  dc exec -T workspace bash -lc "
    python3 - <<'PY'
import hashlib, hmac, json, os, time
from pathlib import Path

def sign(req):
    secret = os.environ.get("FLEET_HANDSHAKE_SECRET") or ""
    body = {k: v for k, v in req.items() if k != "hmac"}
    raw = json.dumps(body, sort_keys=True, separators=(",", ":")).encode()
    body["hmac"] = hmac.new(secret.encode(), raw, hashlib.sha256).hexdigest()
    return body

fleet = Path('/workspace/.fleet')
# plant tools outside jobs
evil = Path('/tmp/evil-tools')
evil.mkdir(exist_ok=True)
(evil / '.fleet-tools.json').write_text(json.dumps({'x': {'argv': ['echo', 'no']}}))
req = sign({
  'job_id': '$JOB_ID',
  'action': 'tool',
  'req_id': '$REQ_ID3',
  'tool': 'x',
  'cwd': str(evil),
})
(fleet / 'request-$REQ_ID3.json').write_text(json.dumps(req))
res = fleet / 'result-$REQ_ID3.json'
for _ in range(100):
    if res.is_file():
        d = json.loads(res.read_text())
        print(json.dumps(d, indent=2))
        assert d.get('ok') is False, d
        assert 'outside job isolation' in (d.get('error') or ''), d
        print('OUTSIDE_CWD_REJECTED')
        break
    time.sleep(0.1)
else:
    raise SystemExit('timeout')
PY
  "

  echo "=== assert job workdir + tools file ==="
  dc exec -T workspace test -f "$CWD/.fleet-tools.json"
  dc exec -T workspace cat "$CWD/.fleet-tools.json"
  echo "COMPOSE_TOOL_INTEGRATION_OK job_id=$JOB_ID cwd=$CWD"
} 2>&1 | tee "$OUT"
status=${PIPESTATUS[0]}
exit $status
