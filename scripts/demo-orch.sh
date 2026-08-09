#!/usr/bin/env bash
# PR9: E2E demo against standalone fleet-orchestrator or fleet-server (/v1).
#
# Env:
#   FLEET_ORCH_URL  default http://127.0.0.1:8081 (standalone orch) or :8080 for server
#   ADMIN_TOKEN     required for /v1 (default matches .env.example)
#   SCENARIO        happy|cancel|all  (default happy)
#
# Examples:
#   ADMIN_TOKEN=dev-admin-token-change-me ./scripts/demo-orch.sh
#   SCENARIO=all FLEET_ORCH_URL=http://127.0.0.1:8081 ./scripts/demo-orch.sh
set -euo pipefail

BASE="${FLEET_ORCH_URL:-http://127.0.0.1:8081}"
TOKEN="${ADMIN_TOKEN:-dev-admin-token-change-me}"
AUTH="Authorization: Bearer ${TOKEN}"
SCENARIO="${SCENARIO:-happy}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURES="$ROOT/crates/fleet-orchestrator/fixtures/plans"

need_jq() {
  if ! command -v jq >/dev/null 2>&1; then
    echo "jq is required for demo-orch.sh" >&2
    exit 1
  fi
}

uuid() {
  uuidgen 2>/dev/null || python3 -c 'import uuid; print(uuid.uuid4())'
}

json_post() {
  local path="$1"
  local body="$2"
  curl -sf -X POST "$BASE$path" -H "$AUTH" -H 'content-type: application/json' -d "$body"
}

json_get() {
  local path="$1"
  curl -sf "$BASE$path" -H "$AUTH"
}

create_project() {
  local name="${1:-demo-$(date +%s)}"
  json_post /v1/projects "{\"name\":\"$name\",\"max_agents\":3}"
}

tick() {
  local pid="$1"
  json_post "/v1/projects/$pid/admin/tick" '{}'
}

# Inject fixture JSON plan with live project/run/plan UUIDs.
post_fixture_file() {
  local pid="$1"
  local file="$2"
  local plan_id run_id payload
  plan_id="$(uuid)"
  run_id="$(uuid)"
  payload=$(jq -c \
    --arg pid "$pid" \
    --arg plan_id "$plan_id" \
    --arg run_id "$run_id" \
    '.plan.project_id = $pid | .plan.plan_id = $plan_id | .plan.run_id = $run_id' \
    "$file")
  json_post "/v1/projects/$pid/runs/fixture" "$payload"
  echo "$run_id"
}

wait_run_status() {
  local pid="$1"
  local run_id="$2"
  local want="$3"
  local max="${4:-12}"
  local i status
  for i in $(seq 1 "$max"); do
    tick "$pid" >/dev/null
    status=$(json_get "/v1/projects/$pid/runs" | jq -r --arg id "$run_id" '
      .runs[] | select(.id == $id) | .status' | head -1)
    echo "  tick $i: run status=$status"
    if [[ "$status" == "$want" ]]; then
      return 0
    fi
    sleep 0.2
  done
  echo "timeout waiting for run $run_id → $want (last=$status)" >&2
  return 1
}

scenario_happy() {
  echo "=== scenario: happy two-task chain ==="
  local proj pid run_id
  proj=$(create_project "demo-happy-$(date +%s)")
  pid=$(echo "$proj" | jq -r .id)
  echo "project_id=$pid"
  run_id=$(post_fixture_file "$pid" "$FIXTURES/two_task_chain.json")
  echo "run_id=$run_id"
  wait_run_status "$pid" "$run_id" completed 8
  echo "tasks:"
  json_get "/v1/projects/$pid/tasks" | jq .
  echo "OK happy"
}

scenario_cancel() {
  echo "=== scenario: cancel mid-run ==="
  local proj pid run_id
  proj=$(create_project "demo-cancel-$(date +%s)")
  pid=$(echo "$proj" | jq -r .id)
  echo "project_id=$pid"
  # Long-ish work if STUB_DURATION_S>0; still cancel after first tick.
  run_id=$(post_fixture_file "$pid" "$FIXTURES/two_task_chain.json")
  echo "run_id=$run_id"
  tick "$pid" | jq .
  json_post "/v1/projects/$pid/runs/$run_id/cancel" '{"reason":"demo cancel"}' | jq .
  wait_run_status "$pid" "$run_id" cancelled 8
  echo "OK cancel"
}

scenario_idempotent() {
  echo "=== scenario: double force-tick idempotency ==="
  local proj pid run_id r1 r2
  proj=$(create_project "demo-idemp-$(date +%s)")
  pid=$(echo "$proj" | jq -r .id)
  run_id=$(post_fixture_file "$pid" "$FIXTURES/two_task_chain.json")
  wait_run_status "$pid" "$run_id" completed 8
  r1=$(tick "$pid")
  r2=$(tick "$pid")
  echo "$r1" | jq '{spawned, plans_materialized, tasks_materialized}'
  echo "$r2" | jq '{spawned, plans_materialized, tasks_materialized}'
  local s1 s2
  s1=$(echo "$r1" | jq -r .spawned)
  s2=$(echo "$r2" | jq -r .spawned)
  if [[ "$s1" != "0" || "$s2" != "0" ]]; then
    echo "expected spawned=0 after complete, got $s1 / $s2" >&2
    exit 1
  fi
  echo "OK idempotent"
}

main() {
  need_jq
  echo "== healthz @ $BASE =="
  curl -sf "$BASE/healthz" | tee /tmp/orch-health.json
  echo

  case "$SCENARIO" in
    happy) scenario_happy ;;
    cancel) scenario_cancel ;;
    idempotent) scenario_idempotent ;;
    all)
      scenario_happy
      scenario_cancel
      scenario_idempotent
      ;;
    *)
      echo "unknown SCENARIO=$SCENARIO (happy|cancel|idempotent|all)" >&2
      exit 1
      ;;
  esac
  echo "done"
}

main "$@"
