#!/usr/bin/env bash
# Run every test on this checkout. Report-then-fail (no || true on cargo).
# No SKIP_* defaults. No WARN-and-continue. Missing host dx / foreign stack = fail.
# Live :8080/:3030 must be THIS worktree (scripts/tree-id.sh baked into API+UI).
# After code changes: install script or --prepare-kind first. Without that,
# this run is not a valid verification.
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

write_ui_git_sha() {
  local sha="$1"
  local dest="${ROOT}/crates/fleet-ui/public/fleet-git-sha.txt"
  mkdir -p "$(dirname "${dest}")"
  printf '%s\n' "${sha}" >"${dest}"
}

host_dx_build() {
  local sha public
  sha="$(bash "${ROOT}/scripts/tree-id.sh")"
  export FLEET_GIT_SHA="${sha}"
  write_ui_git_sha "${sha}"
  dx build --package fleet-ui --platform web
  html="$(find "${CARGO_TARGET_DIR}" "${ROOT}/target" -type f -path '*/web/public/index.html' 2>/dev/null | head -1)"
  public=""
  if [[ -n "${html}" ]]; then
    public="$(dirname "${html}")"
  fi
  if [[ -z "${public}" ]]; then
    echo "ERROR: dx build produced no web/public/index.html" >&2
    return 1
  fi
  printf '%s\n' "${sha}" >"${public}/fleet-git-sha.txt"
  test "$(tr -d '[:space:]' <"${public}/fleet-git-sha.txt")" = "${sha}"
}

image_label_sha() {
  docker image inspect -f '{{index .Config.Labels "fleet.git_sha"}}' "$1" 2>/dev/null || true
}

# Same skip policy as install docker_build_if_needed: label == tree-id.sh.
# Tag existence, leftover FLEET_GIT_SHA env, or git rev-parse HEAD is not enough.
docker_build_if_needed() {
  local tag="$1"
  shift
  local sha have
  sha="$(bash "${ROOT}/scripts/tree-id.sh")"
  if [[ "${FORCE_REBUILD:-}" != "1" ]] && docker image inspect "${tag}" >/dev/null 2>&1; then
    have="$(image_label_sha "${tag}")"
    if [[ "${have}" == "${sha}" ]]; then
      log "skip ${tag} (fleet.git_sha=${sha})"
      return 0
    fi
    log "rebuild ${tag}: image fleet.git_sha=${have:-unset} != ${sha}"
  fi
  log "==== docker build ${tag} FLEET_GIT_SHA=${sha} ===="
  DOCKER_BUILDKIT=1 docker build -t "${tag}" --build-arg "FLEET_GIT_SHA=${sha}" "$@"
}

prepare_kind() {
  local sha
  sha="$(bash "${ROOT}/scripts/tree-id.sh")"
  export FLEET_GIT_SHA="${sha}"
  export DOCKER_BUILDKIT=1
  write_ui_git_sha "${sha}"
  docker_build_if_needed agent-fleet-api:local -f "${ROOT}/Dockerfile" "${ROOT}"
  docker_build_if_needed agent-fleet-ui:local -f "${ROOT}/Dockerfile.ui" "${ROOT}"
  docker_build_if_needed agent-fleet-operator:local -f "${ROOT}/Dockerfile.operator" "${ROOT}"
  if python3 "${ROOT}/tests/browser/stack_identity.py"; then
    log "Kind host :8080/:3030 is this tree (API + UI sha) — skip load/rollout"
    return 0
  fi
  kind load docker-image agent-fleet-api:local agent-fleet-ui:local agent-fleet-operator:local \
    --name agent-fleet
  kubectl --context kind-agent-fleet apply -k "${ROOT}/infra/overlays/kind"
  kubectl --context kind-agent-fleet -n agent-fleet rollout restart \
    deploy/api deploy/ui deploy/fleet-operator deploy/fleet-host
  kubectl --context kind-agent-fleet -n agent-fleet rollout status deploy/api --timeout=300s
  kubectl --context kind-agent-fleet -n agent-fleet rollout status deploy/ui --timeout=300s
  kubectl --context kind-agent-fleet -n agent-fleet rollout status deploy/fleet-host --timeout=300s
  local i
  for i in $(seq 1 60); do
    if python3 "${ROOT}/tests/browser/stack_identity.py"; then
      log "Kind host :8080/:3030 is this tree (API + UI sha)"
      return 0
    fi
    sleep 3
  done
  log "ERROR: Kind host :8080/:3030 still not this tree after rebuild"
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
if [[ "${FLEET_ALLOW_FOREIGN_STACK:-}" == "1" ]]; then
  echo "ERROR: FLEET_ALLOW_FOREIGN_STACK is not a green path. Unset it." >&2
  exit 1
fi
unset FLEET_ALLOW_FOREIGN_STACK || true
export DATABASE_URL="${DATABASE_URL:-postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable}"
export JWT_SECRET="${JWT_SECRET:-dev-change-me-to-a-long-random-secret-at-least-32-chars}"
export ADMIN_TOKEN="${ADMIN_TOKEN:-test-admin}"
# Always this worktree. Do not let .env / kubectl leftovers override the bake.
export FLEET_GIT_SHA
FLEET_GIT_SHA="$(bash "${ROOT}/scripts/tree-id.sh")"

if ! python3 "${ROOT}/tests/browser/stack_identity.py" --tools; then
  echo "ERROR: required tools missing (host dx included). Run the install script." >&2
  echo "EVERY-TEST FAILED" | tee "${SUM}"
  exit 1
fi

run_step fail-closed-contract \
  python3 -m unittest tests.browser.test_fail_closed_contract -q
run_step unify-sku-contract \
  python3 -m unittest tests.browser.test_unify_sku_contract -q
run_step host-dx-build host_dx_build
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
run_step human-surface-contract \
  python3 -m unittest tests.browser.test_human_surface_contract -q
run_step human-test-scope \
  python3 tests/browser/surface_scope.py --check
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
run_step browser-e2e-cases \
  python3 -m unittest tests.browser.test_cases_e2e -v
run_step browser-e2e-inbox \
  python3 -m unittest tests.browser.test_inbox_e2e -v
run_step browser-e2e-settings-members \
  python3 -m unittest tests.browser.test_settings_members_e2e -v
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
