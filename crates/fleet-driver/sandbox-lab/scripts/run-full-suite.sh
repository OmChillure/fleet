#!/usr/bin/env bash
# Run the fleet-driver test suite (does not install packages).
# Prereq: install-full-suite-deps.sh (sudo) once.
set -euo pipefail

LAB="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(cd "${LAB}/../../.." && pwd)"
SCRIPTS="${LAB}/scripts"
cd "${REPO}"

SKIP_LLM="${SKIP_LLM:-0}"
SKIP_COMPOSE="${SKIP_COMPOSE:-0}"
FAIL=0
pass() { echo "PASS  $*"; }
fail() { echo "FAIL  $*"; FAIL=1; }

echo "==== cargo test -p fleet-driver ===="
if cargo test -p fleet-driver --offline; then
  pass cargo
else
  fail cargo
fi

echo "==== workspace_agent protocol ===="
if python3 "${LAB}/workspace/tests/test_workspace_agent_protocol.py" -q; then
  pass protocol
else
  fail protocol
fi

echo "==== host e2e (no docker/llm) ===="
if python3 "${SCRIPTS}/run-host-e2e.py"; then
  pass host-e2e
else
  fail host-e2e
fi

if [[ "${SKIP_COMPOSE}" != "1" ]]; then
  echo "==== compose tool integration ===="
  if "${SCRIPTS}/run-compose-tool-integration.sh" /tmp/fleet-compose-int.log; then
    pass compose-int
  else
    fail compose-int
    tail -20 /tmp/fleet-compose-int.log || true
  fi
fi

if [[ "${SKIP_LLM}" != "1" ]]; then
  echo "==== invoices opencode (LLM) ===="
  # Prefer caller env; .env is local and gitignored.
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
