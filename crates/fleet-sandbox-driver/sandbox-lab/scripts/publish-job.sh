#!/usr/bin/env bash
# Publish an analyze_repo job to Redis (host side).
# Usage:
#   ./scripts/publish-job.sh
#   ./scripts/publish-job.sh config/job.example.json
#   JOB_JSON=path ./scripts/publish-job.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [[ -f .env ]]; then
  # shellcheck disable=SC1091
  set -a
  source .env
  set +a
fi

REDIS_URL_HOST="${REDIS_URL_HOST:-redis://127.0.0.1:6379/0}"
QUEUE="${FLEET_JOBS_QUEUE:-fleet:jobs}"
JOB_FILE="${1:-${JOB_JSON:-config/job.example.json}}"

if [[ ! -f "$JOB_FILE" ]]; then
  echo "job file not found: $JOB_FILE" >&2
  echo "copy config/job.example.json and fill repo/model, or pass a path" >&2
  exit 1
fi

python3 - "$REDIS_URL_HOST" "$QUEUE" "$JOB_FILE" <<'PY'
import json, sys, uuid
import urllib.parse

redis_url, queue, path = sys.argv[1], sys.argv[2], sys.argv[3]

try:
    import redis
except ImportError:
    print("Install redis on host: pip install redis", file=sys.stderr)
    sys.exit(2)

with open(path, encoding="utf-8") as f:
    job = json.load(f)

# Overlay from environment if placeholders left
import os
def fill(key, env_key, default=None):
    val = job.get(key)
    if val is None or (isinstance(val, str) and ("YOUR_" in val or val == "replace-with-uuid-or-leave-for-script")):
        job[key] = os.environ.get(env_key, default if default is not None else val)

fill("repo_url", "GIT_REPO_URL")
fill("git_ref", "GIT_REF", "main")
fill("model", "MODEL")
if not job.get("model"):
    fill("model", "OPENCODE_MODEL")
if not job.get("model"):
    fill("model", "LMSTUDIO_MODEL")
fill("harness", "HARNESS", "opencode")
# back-compat lmstudio fields if still present in old job files
fill("lmstudio_base_url", "OPENCODE_BASE_URL")
if not job.get("lmstudio_base_url"):
    fill("lmstudio_base_url", "LMSTUDIO_BASE_URL")
fill("lmstudio_api_key", "OPENCODE_API_KEY", "lm-studio")
if not job.get("lmstudio_api_key"):
    fill("lmstudio_api_key", "LMSTUDIO_API_KEY", "lm-studio")
if not job.get("job_id") or "replace" in str(job.get("job_id")):
    job["job_id"] = str(uuid.uuid4())
if os.environ.get("ANALYZE_PROMPT") and (
    not job.get("prompt") or "Analyze this repository" in job.get("prompt", "")
):
    # keep job prompt unless env explicitly set and job still default-ish
    pass
if os.environ.get("ANALYZE_PROMPT"):
    job["prompt"] = os.environ["ANALYZE_PROMPT"]

r = redis.Redis.from_url(redis_url, decode_responses=True)
r.ping()
payload = json.dumps(job)
r.lpush(queue, payload)
print(json.dumps({"published": True, "queue": queue, "job_id": job["job_id"], "repo_url": job.get("repo_url"), "model": job.get("model")}, indent=2))
print(f"\nWait with: ./scripts/wait-result.sh {job['job_id']}")
PY
