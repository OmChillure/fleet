#!/usr/bin/env bash
# One task sandbox (own project + volume). Quiet by default.
#   VERBOSE=1           docker + full driver logs
#   FULL_TRANSCRIPT=1   full model transcript
#   TRANSCRIPT_CHARS=N  truncate length (default 1200)
#   KEEP_SANDBOX=1      leave containers/volume after run
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SCRIPTS="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"
VERBOSE="${VERBOSE:-0}"

log() { echo "$*"; }

if [[ ! -f .env ]]; then
  cp .env.example .env
  log "created .env — set OPENCODE_BASE_URL / MODEL / SSH_KEY_HOST_PATH"
fi
while IFS= read -r line || [[ -n "$line" ]]; do
  [[ -z "$line" || "$line" =~ ^[[:space:]]*# ]] && continue
  if [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]]; then
    export "${BASH_REMATCH[1]}=${BASH_REMATCH[2]}"
  fi
done < .env

JOB_FILE="${1:-config/job.123-invoices.json}"
[[ -f "$JOB_FILE" ]] || { echo "job file not found: $JOB_FILE" >&2; exit 1; }
export JOB_FILE

# Harness is per-task (from job JSON). .env HARNESS is only a fallback when job omits it.
JOB_HARNESS="$(python3 -c 'import json,os; from pathlib import Path; print((json.loads(Path(os.environ["JOB_FILE"]).read_text()).get("harness") or "").strip())')"
if [[ -z "$JOB_HARNESS" ]]; then
  JOB_HARNESS="${HARNESS:-opencode}"
fi
export HARNESS="$JOB_HARNESS"
# Driver image must match this task's harness (task A opencode, task B pi, …)
if [[ -z "${DRIVER_BASE_IMAGE:-}" ]]; then
  case "$(printf '%s' "$HARNESS" | tr '[:upper:]' '[:lower:]')" in
    pi|pi-dev|pi_acp) export DRIVER_BASE_IMAGE=agent-fleet-pi:alpine ;;
    codex)            export DRIVER_BASE_IMAGE=agent-fleet-codex:alpine ;;
    grok)             export DRIVER_BASE_IMAGE=agent-fleet-grok:alpine ;;
    *)                export DRIVER_BASE_IMAGE=agent-fleet-opencode:alpine ;;
  esac
fi
if [[ "$VERBOSE" == "1" ]]; then
  export RUST_LOG="${RUST_LOG:-info}"
else
  export RUST_LOG="${RUST_LOG:-warn,fleet_driver=info}"
fi

# Harness base image must exist locally (compose will otherwise try docker.io and fail)
AGENTS_DIR="$(cd "$ROOT/../agents" && pwd)"
ensure_base_image() {
  local img="$1"
  if docker image inspect "$img" >/dev/null 2>&1; then
    return 0
  fi
  log "missing image $img — building (once)…"
  case "$img" in
    agent-fleet-pi:alpine|agent-fleet-pi:*)
      if ! docker image inspect agent-fleet-base:alpine >/dev/null 2>&1; then
        docker build -t agent-fleet-base:alpine -f "$AGENTS_DIR/base/Dockerfile" "$AGENTS_DIR/base"
      fi
      docker build -t agent-fleet-pi:alpine \
        --build-arg BASE_IMAGE=agent-fleet-base:alpine \
        -f "$AGENTS_DIR/pi/Dockerfile" "$AGENTS_DIR/pi"
      ;;
    agent-fleet-opencode:alpine|agent-fleet-opencode:*)
      if ! docker image inspect agent-fleet-base:alpine >/dev/null 2>&1; then
        docker build -t agent-fleet-base:alpine -f "$AGENTS_DIR/base/Dockerfile" "$AGENTS_DIR/base"
      fi
      docker build -t agent-fleet-opencode:alpine \
        --build-arg BASE_IMAGE=agent-fleet-base:alpine \
        -f "$AGENTS_DIR/opencode/Dockerfile" "$AGENTS_DIR/opencode"
      ;;
    *)
      echo "base image not found: $img" >&2
      echo "build harness images: $AGENTS_DIR/build.sh" >&2
      exit 1
      ;;
  esac
}
ensure_base_image "$DRIVER_BASE_IMAGE"

JOB_ID="$(python3 -c 'import json,uuid,os; from pathlib import Path; j=json.loads(Path(os.environ["JOB_FILE"]).read_text()); print(j.get("job_id") or uuid.uuid4())')"
export JOB_ID
SHORT="${JOB_ID//-/}"
SHORT="${SHORT:0:12}"
export COMPOSE_PROJECT_NAME="${COMPOSE_PROJECT_NAME:-fleet-task-${SHORT}}"
export FLEET_LAB_JOB_ID="$JOB_ID"
export FLEET_LAB_COMPOSE_PROJECT="$COMPOSE_PROJECT_NAME"
# Per-task state (avoid /tmp races when multiple sandboxes run)
STATE_DIR="${TMPDIR:-/tmp}/fleet-lab-runs/${COMPOSE_PROJECT_NAME}"
mkdir -p "$STATE_DIR"
export FLEET_LAB_RESULT_FILE="$STATE_DIR/result.json"
echo "$JOB_ID" > "$STATE_DIR/job_id"
echo "$COMPOSE_PROJECT_NAME" > "$STATE_DIR/compose_project"
# Legacy paths for dual summary (last finished task)
echo "$JOB_ID" > /tmp/fleet-lab-job-id
echo "$COMPOSE_PROJECT_NAME" > /tmp/fleet-lab-compose-project

HARNESS_HINT="$HARNESS"
MODEL_HINT="${MODEL:-}"
if [[ -z "$MODEL_HINT" ]]; then
  MODEL_HINT="$(python3 -c 'import json,os; from pathlib import Path; print(json.loads(Path(os.environ["JOB_FILE"]).read_text()).get("model") or "")')"
fi

dc() { docker compose -p "$COMPOSE_PROJECT_NAME" "$@"; }

cleanup() {
  local code=$?
  if [[ "${KEEP_SANDBOX:-0}" == "1" ]]; then
    log "kept sandbox  project=$COMPOSE_PROJECT_NAME"
  else
    dc down -v --remove-orphans >/dev/null 2>&1 || true
  fi
  exit "$code"
}
trap cleanup EXIT

log "task  job=$JOB_ID  harness=$HARNESS_HINT  model=${MODEL_HINT:-?}  project=$COMPOSE_PROJECT_NAME"

# Build driver against local base image (pull=false avoids docker.io for agent-fleet-*)
if [[ "$VERBOSE" == "1" ]]; then
  dc build --pull=false driver
  dc up -d redis workspace
else
  if ! dc build --pull=false driver >/tmp/fleet-lab-driver-build.log 2>&1; then
    echo "driver build failed (BASE_IMAGE=$DRIVER_BASE_IMAGE). last lines:" >&2
    tail -40 /tmp/fleet-lab-driver-build.log >&2
    exit 1
  fi
  dc up -d redis workspace >/dev/null 2>&1
fi

for _ in $(seq 1 60); do
  if dc exec -T workspace test -f /workspace/.fleet/agent.alive 2>/dev/null; then
    break
  fi
  sleep 0.25
done

dc exec -T redis redis-cli DEL "${FLEET_JOBS_QUEUE:-fleet:jobs}" >/dev/null 2>&1 || true
python3 "$SCRIPTS/_publish_lab_job.py"

log "driver…"
# Pass this task's harness into the container (do not use stale .env HARNESS alone)
if [[ "$VERBOSE" == "1" ]]; then
  dc run --rm -e RUST_LOG -e "HARNESS=$HARNESS" driver
else
  set +o pipefail
  dc run --rm -e RUST_LOG -e "HARNESS=$HARNESS" driver 2>&1 | grep -E \
    '=== job ===|provision complete|ACP prompt complete|ACP done|published result|ERROR|job failed|WARN' \
    || true
  set -o pipefail
fi

# Snapshot result while this task's redis is still up (before teardown trap)
log "result"
if raw="$(dc exec -T redis redis-cli GET "fleet:result:${JOB_ID}" 2>/dev/null | tr -d '\r')"; then
  if [[ -n "$raw" && "$raw" != "(nil)" ]]; then
    printf '%s\n' "$raw" > "$FLEET_LAB_RESULT_FILE"
  fi
fi
python3 "$SCRIPTS/_print_lab_result.py"
log "done"
