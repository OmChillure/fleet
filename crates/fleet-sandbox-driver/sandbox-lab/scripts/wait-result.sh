#!/usr/bin/env bash
# Wait for a job result on Redis.
# Usage:
#   ./scripts/wait-result.sh                 # next result on fleet:results
#   ./scripts/wait-result.sh <job_id>        # poll fleet:result:{id}
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
if [[ -f .env ]]; then
  set -a
  # shellcheck disable=SC1091
  source .env
  set +a
fi
REDIS_URL_HOST="${REDIS_URL_HOST:-redis://127.0.0.1:6379/0}"
RESULTS="${FLEET_RESULTS_QUEUE:-fleet:results}"
JOB_ID="${1:-}"
TIMEOUT="${WAIT_TIMEOUT_SEC:-900}"

python3 - "$REDIS_URL_HOST" "$RESULTS" "$JOB_ID" "$TIMEOUT" <<'PY'
import json, sys, time
redis_url, results_q, job_id, timeout_s = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
try:
    import redis
except ImportError:
    print("pip install redis", file=sys.stderr)
    sys.exit(2)

r = redis.Redis.from_url(redis_url, decode_responses=True)
deadline = time.time() + timeout_s

if job_id:
    key = f"fleet:result:{job_id}"
    print(f"waiting for {key} (timeout {timeout_s}s)…", flush=True)
    while time.time() < deadline:
        val = r.get(key)
        if val:
            data = json.loads(val)
            acp = data.get("acp") or data.get("opencode") or {}
            summary = {k: data[k] for k in data if k not in ("opencode", "acp", "analysis")}
            summary["model"] = acp.get("model")
            summary["session_id"] = acp.get("session_id")
            summary["stop_reason"] = acp.get("stop_reason")
            print(json.dumps(summary, indent=2))
            print("\n=== model did (transcript) ===\n")
            print(data.get("analysis") or data.get("error") or "(empty)")
            print("\n=== end transcript ===")
            if acp.get("stderr_tail"):
                print("\n--- harness stderr (tail) ---\n")
                print(acp["stderr_tail"][-2000:])
            sys.exit(0 if data.get("ok") else 1)
        time.sleep(1)
    print("timeout", file=sys.stderr)
    sys.exit(1)
else:
    print(f"BLPOP {results_q} (timeout {timeout_s}s)…", flush=True)
    item = r.blpop(results_q, timeout=timeout_s)
    if not item:
        print("timeout", file=sys.stderr)
        sys.exit(1)
    _, raw = item
    data = json.loads(raw)
    acp = data.get("acp") or data.get("opencode") or {}
    summary = {k: data[k] for k in data if k not in ("opencode", "acp", "analysis")}
    summary["model"] = acp.get("model")
    summary["session_id"] = acp.get("session_id")
    summary["stop_reason"] = acp.get("stop_reason")
    print(json.dumps(summary, indent=2))
    print("\n=== model did (transcript) ===\n")
    print(data.get("analysis") or data.get("error") or "(empty)")
    print("\n=== end transcript ===")
    sys.exit(0 if data.get("ok") else 1)
PY
