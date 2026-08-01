#!/usr/bin/env bash
# Smoke demo against a running fleet-server (orchestrator mounted) or standalone orch.
set -euo pipefail
BASE="${FLEET_ORCH_URL:-http://127.0.0.1:8080}"
TOKEN="${ADMIN_TOKEN:-dev-admin-token-change-me}"
AUTH="Authorization: Bearer ${TOKEN}"

echo "== healthz =="
curl -sf "$BASE/healthz" | tee /tmp/orch-health.json
echo

echo "== create project =="
PROJ=$(curl -sf -X POST "$BASE/v1/projects" -H "$AUTH" -H 'content-type: application/json' \
  -d "{\"name\":\"demo-$(date +%s)\",\"max_agents\":3}")
echo "$PROJ"
PID=$(echo "$PROJ" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1)
echo "project_id=$PID"

RUN_ID=$(uuidgen 2>/dev/null || python3 -c 'import uuid; print(uuid.uuid4())')
PLAN_ID=$(uuidgen 2>/dev/null || python3 -c 'import uuid; print(uuid.uuid4())')

echo "== fixture =="
curl -sf -X POST "$BASE/v1/projects/$PID/runs/fixture" -H "$AUTH" -H 'content-type: application/json' \
  -d "{\"plan\":{\"plan_id\":\"$PLAN_ID\",\"project_id\":\"$PID\",\"run_id\":\"$RUN_ID\",\"version\":1,\"status\":\"accepted\",\"source\":\"fixture\",\"tasks\":[{\"plan_task_id\":\"t1\",\"title\":\"Root\",\"depends_on\":[],\"worker_profile\":\"coding\",\"priority\":100,\"max_retries\":1},{\"plan_task_id\":\"t2\",\"title\":\"Child\",\"depends_on\":[\"t1\"],\"worker_profile\":\"coding\",\"priority\":50}],\"policy\":{\"on_task_fail\":\"fail_run\",\"auto_sync_external\":false}}}"
echo

for i in 1 2 3 4; do
  echo "== tick $i =="
  curl -sf -X POST "$BASE/v1/projects/$PID/admin/tick" -H "$AUTH" -H 'content-type: application/json' -d '{}'
  echo
done

echo "== tasks =="
curl -sf "$BASE/v1/projects/$PID/tasks" -H "$AUTH"
echo
echo "done"
