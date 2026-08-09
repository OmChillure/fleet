#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
if [[ ! -f .env ]]; then
  echo "copy .env.example → .env first" >&2
  exit 1
fi
set -a
# shellcheck disable=SC1091
source .env
set +a

TMP="$(mktemp)"
python3 - <<'PY' > "$TMP"
import json, os, uuid
provider_id = os.environ.get("OPENCODE_PROVIDER_ID")
base = os.environ.get("OPENCODE_BASE_URL") or os.environ.get("LMSTUDIO_BASE_URL")
model = os.environ.get("MODEL") or os.environ.get("OPENCODE_MODEL") or os.environ.get("LMSTUDIO_MODEL")
# Align with fleet-driver seed_job_from_env / policy defaults:
# - work_dir omitted → /workspace/jobs/{job_id}/repo
# - auto_approve false, empty permission_allow (default-deny)
job = {
  "job_id": str(uuid.uuid4()),
  "type": "analyze_repo",
  "harness": os.environ.get("HARNESS", "opencode"),
  "repo_url": os.environ["GIT_REPO_URL"],
  "git_ref": os.environ.get("GIT_REF", "main"),
  "prompt": os.environ.get("ANALYZE_PROMPT", "Analyze this repository briefly."),
  "model": model,
  "timeout_sec": int(os.environ.get("JOB_TIMEOUT_SEC", "600")),
  "auto_approve": False,
  "permission_allow": [],
}
if provider_id and base:
    mid = (model or "model")
    if mid.startswith(provider_id + "/"):
        mid = mid[len(provider_id)+1:]
    job["provider"] = {
        "id": provider_id,
        "name": os.environ.get("OPENCODE_PROVIDER_NAME", provider_id),
        "npm": os.environ.get("OPENCODE_PROVIDER_NPM", "@ai-sdk/openai-compatible"),
        "base_url": base,
        "api_key_env": os.environ.get("OPENCODE_API_KEY_ENV", "OPENCODE_API_KEY"),
        "api_key": os.environ.get("OPENCODE_API_KEY") or os.environ.get("LMSTUDIO_API_KEY") or "lm-studio",
        "models": {mid: {"name": mid}},
    }
print(json.dumps(job, indent=2))
PY
echo "job payload:"
cat "$TMP"
"$ROOT/scripts/publish-job.sh" "$TMP"
rm -f "$TMP"
