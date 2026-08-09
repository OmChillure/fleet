#!/usr/bin/env bash
# Run every test on this checkout. Report-then-fail (no || true on cargo).
# No SKIP_* defaults. Kind :8080 must be THIS tree (git_sha == HEAD).
#
#   ./scripts/bootstrap-every-test.sh
#   ./scripts/run-every-test.sh --prepare-kind   # rebuild/load api+ui+operator
#   ./scripts/run-every-test.sh
set -u
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"
export PATH="${HOME}/.cargo/bin:${HOME}/.grok/bin:${PATH}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT}/target}"
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-${ROOT}/.tmp/ms-playwright}"
LAB="${ROOT}/crates/fleet-sandbox-driver/sandbox-lab"
LOG_DIR="${ROOT}/.tmp"
mkdir -p "${LOG_DIR}"
LOG="${LOG_DIR}/every-test.log"
SUM="${LOG_DIR}/every-test-summary.txt"
: >"${LOG}"
FAIL=0

log() { printf '%s\n' "$*" | tee -a "${LOG}"; }
pass() { log "PASS  $*"; }
fail() { log "FAIL  $*"; FAIL=1; }

run_step() {
  local name="$1"
  shift
  log "==== ${name} ===="
  if "$@" >>"${LOG}" 2>&1; then
    pass "${name}"
    return 0
  fi
  fail "${name}"
  tail -40 "${LOG}" | tee -a "${SUM}" >/dev/null || true
  return 0
}

prepare_kind() {
  local sha
  sha="$(git -C "${ROOT}" rev-parse HEAD)"
  export FLEET_GIT_SHA="${sha}"
  log "==== docker build api/ui/operator FLEET_GIT_SHA=${sha} ===="
  docker build --build-arg "FLEET_GIT_SHA=${sha}" \
    -t agent-fleet-api:local -f "${ROOT}/Dockerfile" "${ROOT}"
  docker build -t agent-fleet-ui:local -f "${ROOT}/Dockerfile.ui" "${ROOT}"
  docker build -t agent-fleet-operator:local -f "${ROOT}/Dockerfile.operator" "${ROOT}"
  kind load docker-image agent-fleet-api:local agent-fleet-ui:local agent-fleet-operator:local \
    --name agent-fleet
  kubectl --context kind-agent-fleet -n agent-fleet rollout restart deploy/api deploy/ui deploy/fleet-operator
  kubectl --context kind-agent-fleet -n agent-fleet rollout status deploy/api --timeout=300s
  kubectl --context kind-agent-fleet -n agent-fleet rollout status deploy/ui --timeout=300s
  # Wait host publish
  local i
  for i in $(seq 1 60); do
    if python3 "${ROOT}/tests/browser/stack_identity.py"; then
      log "Kind host :8080 is this tree"
      return 0
    fi
    sleep 3
  done
  log "ERROR: Kind host :8080 still not this tree after rebuild"
  return 1
}

if [[ "${1:-}" == "--prepare-kind" ]]; then
  set -e
  prepare_kind
  exit 0
fi

set +e
if [[ -f "${ROOT}/.env" ]]; then
  set -a
  # shellcheck disable=SC1091
  . "${ROOT}/.env"
  set +a
fi
export DATABASE_URL="${DATABASE_URL:-postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable}"
export JWT_SECRET="${JWT_SECRET:-dev-change-me-to-a-long-random-secret-at-least-32-chars}"
export ADMIN_TOKEN="${ADMIN_TOKEN:-test-admin}"
export FLEET_GIT_SHA="${FLEET_GIT_SHA:-$(git -C "${ROOT}" rev-parse HEAD)}"

run_step stack-identity python3 "${ROOT}/tests/browser/stack_identity.py"
run_step cargo-workspace-lib \
  cargo test --workspace --lib --exclude fleet-ui --locked
run_step cargo-sandbox-driver \
  cargo test -p fleet-sandbox-driver --locked
run_step cargo-driver \
  cargo test -p fleet-driver --locked
run_step cargo-driver-acp-grok \
  cargo test -p fleet-driver --locked --test acp_grok -- --ignored --nocapture
run_step orch-integration \
  cargo test -p fleet-orchestrator --locked \
    --test store_integration --test tick_integration --test api_integration \
    --test e2e_matrix --test process_sandbox
run_step fleet-auth-tests \
  cargo test -p fleet-auth --locked --tests
run_step fleet-server-tests \
  cargo test -p fleet-server --locked --tests
run_step fleet-db-tests \
  cargo test -p fleet-db --locked --tests
run_step record-demo-contract \
  python3 -m unittest tests.browser.test_record_demo_contract -q
run_step workspace-protocol \
  python3 "${LAB}/workspace/tests/test_workspace_agent_protocol.py" -q
run_step lab-python-discover \
  python3 -m unittest discover -s "${LAB}/tests" -p 'test_*.py' -q
run_step compose-int \
  "${LAB}/scripts/run-compose-tool-integration.sh" "${LOG_DIR}/compose-int.log"
run_step invoices-opencode \
  env VERBOSE="${VERBOSE:-0}" bash -lc "cd '${LAB}' && ./scripts/run-invoices-example.sh config/job.123-invoices.opencode.json"
run_step browser-e2e \
  python3 "${ROOT}/tests/browser/test_app_e2e.py" -v
run_step record-demo-core \
  python3 "${ROOT}/tests/browser/record_demo.py" landing auth api chat

{
  echo
  echo "==== SUMMARY ===="
  grep -E '^(PASS|FAIL)  ' "${LOG}" || true
  if [[ "${FAIL}" -ne 0 ]]; then
    echo "EVERY-TEST FAILED"
  else
    echo "EVERY-TEST OK"
  fi
} | tee "${SUM}" | tee -a "${LOG}"

exit "${FAIL}"
