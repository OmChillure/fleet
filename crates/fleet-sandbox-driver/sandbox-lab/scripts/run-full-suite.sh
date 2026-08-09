#!/usr/bin/env bash
# Run the fleet-sandbox-driver suite (does not install packages).
# Prereq: crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh once.
# Reads repo-root .env (PROFILE / ENABLE_* / SKIP_*). Browser e2e runs only when
# ENABLE_BROWSER_E2E=1 (install default: PROFILE=full only).
set -euo pipefail

LAB="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(cd "${LAB}/../../.." && pwd)"
SCRIPTS="${LAB}/scripts"
ROOT_ENV="${REPO}/.env"
FLEET_TMP="${FLEET_TMP:-${REPO}/.tmp}"
export FLEET_TMP
mkdir -p "${FLEET_TMP}"
cd "${REPO}"

if [[ -f "${ROOT_ENV}" ]]; then
  echo "==> loading ${ROOT_ENV}"
  set -a
  # shellcheck disable=SC1090
  . "${ROOT_ENV}"
  set +a
fi

SKIP_LLM="${SKIP_LLM:-0}"
SKIP_COMPOSE="${SKIP_COMPOSE:-0}"
SKIP_KIND="${SKIP_KIND:-0}"
SKIP_ORCH="${SKIP_ORCH:-0}"
SKIP_BROWSER="${SKIP_BROWSER:-0}"
ENABLE_BROWSER_E2E="${ENABLE_BROWSER_E2E:-0}"
FAIL=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; FAIL=1; }

echo "==== cargo test --workspace --lib (exclude fleet-ui WASM) ===="
if cargo test --workspace --lib --exclude fleet-ui --locked --offline; then
  pass cargo-workspace-lib
else
  fail cargo-workspace-lib
fi

echo "==== cargo test -p fleet-sandbox-driver ===="
if cargo test -p fleet-sandbox-driver --offline --locked; then
  pass cargo-sandbox-driver
else
  fail cargo-sandbox-driver
fi

echo "==== cargo test -p fleet-driver (process worker) ===="
if cargo test -p fleet-driver --offline --locked; then
  pass cargo-driver
else
  fail cargo-driver
fi

echo "==== workspace_agent protocol ===="
if python3 "${LAB}/workspace/tests/test_workspace_agent_protocol.py" -q; then
  pass protocol
else
  fail protocol
fi

echo "==== install contract + kustomize + host/kind/gitssh (skip if absent) ===="
if python3 -m unittest discover -s "${LAB}/tests" -p 'test_*.py' -q; then
  pass lab-python
else
  fail lab-python
fi

echo "==== host e2e (no docker/llm) ===="
if python3 "${SCRIPTS}/run-host-e2e.py"; then
  pass host-e2e
else
  fail host-e2e
fi

if [[ "${SKIP_COMPOSE}" != "1" ]]; then
  echo "==== compose tool integration ===="
  if "${SCRIPTS}/run-compose-tool-integration.sh" "${FLEET_TMP}/compose-int.log"; then
    pass compose-int
  else
    fail compose-int
    tail -20 "${FLEET_TMP}/compose-int.log" || true
  fi
fi

if [[ "${SKIP_ORCH}" != "1" ]]; then
  echo "==== fleet-orchestrator integration (needs DATABASE_URL / :5433) ===="
  export DATABASE_URL="${DATABASE_URL:-postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable}"
  export JWT_SECRET="${JWT_SECRET:-dev-change-me-to-a-long-random-secret-at-least-32-chars}"
  export ADMIN_TOKEN="${ADMIN_TOKEN:-test-admin}"
  if cargo test -p fleet-orchestrator --locked --offline \
      --test store_integration --test tick_integration --test api_integration \
      --test e2e_matrix --test process_sandbox; then
    pass orch-integration
  else
    fail orch-integration
  fi
  echo "==== fleet-auth + fleet-server HTTP integration ===="
  if cargo test -p fleet-auth --locked --offline --tests \
     && cargo test -p fleet-server --locked --offline --tests; then
    pass auth-server-integration
  else
    fail auth-server-integration
  fi
fi

if [[ "${SKIP_KIND}" != "1" ]]; then
  echo "==== kind smoke (skips inside unittest if no cluster) ===="
  if python3 "${LAB}/tests/test_kind_smoke.py" -q; then
    pass kind-smoke
  else
    fail kind-smoke
  fi
fi

# After kind so PROFILE=full extraPortMappings (:3030/:8080) are up first.
if [[ "${ENABLE_BROWSER_E2E}" == "1" && "${SKIP_BROWSER}" != "1" ]]; then
  echo "==== headless browser e2e (needs :3030 UI + :8080 API) ===="
  export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-${FLEET_TMP}/ms-playwright}"
  if python3 "${REPO}/tests/browser/test_app_e2e.py" -q; then
    pass browser-e2e
  else
    fail browser-e2e
  fi
elif [[ "${ENABLE_BROWSER_E2E}" == "1" ]]; then
  echo "==== browser e2e skipped (SKIP_BROWSER=1) ===="
else
  echo "==== browser e2e skipped (ENABLE_BROWSER_E2E!=1) ===="
fi

if [[ "${SKIP_LLM}" != "1" ]]; then
  echo "==== invoices opencode (LLM) ===="
  set -a
  # shellcheck disable=SC1091
  [[ -f "${LAB}/.env" ]] && source "${LAB}/.env"
  set +a
  if (cd "${LAB}" && VERBOSE="${VERBOSE:-0}" ./scripts/run-invoices-example.sh config/job.123-invoices.opencode.json); then
    pass invoices-opencode
  else
    fail invoices-opencode
  fi
fi

echo
if [[ "${FAIL}" -ne 0 ]]; then
  echo "SUITE FAILED"
  exit 1
fi
echo "SUITE OK"
