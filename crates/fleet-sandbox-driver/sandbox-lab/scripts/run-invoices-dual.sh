#!/usr/bin/env bash
# Two sandboxes sequential. Quiet summary.
# Ensures local harness images exist (opencode + pi) before running.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
SCRIPTS="$(cd "$(dirname "$0")" && pwd)"
AGENTS="$(cd "$ROOT/../agents" && pwd)"
REPO="$(cd "$ROOT/../../.." && pwd)"
FLEET_TMP="${FLEET_TMP:-${REPO}/.tmp}"
export FLEET_TMP
mkdir -p "${FLEET_TMP}"
[[ -f .env ]] || cp .env.example .env

echo "dual invoices (2 sandboxes)"
echo

# Preflight: both harness bases (avoids docker.io pull of agent-fleet-pi)
need_build=0
for img in agent-fleet-opencode:alpine agent-fleet-pi:alpine; do
  if ! docker image inspect "$img" >/dev/null 2>&1; then
    echo "missing $img"
    need_build=1
  fi
done
if [[ $need_build -eq 1 ]]; then
  echo "building harness images (base + opencode + pi)…"
  # Prefer full script; falls back to stepwise if needed
  if [[ -x "$AGENTS/build.sh" ]]; then
    # Build only what we need: base, opencode, pi (not full grok/codex if slow)
    docker image inspect agent-fleet-base:alpine >/dev/null 2>&1 \
      || docker build -t agent-fleet-base:alpine -f "$AGENTS/base/Dockerfile" "$AGENTS/base"
    docker image inspect agent-fleet-opencode:alpine >/dev/null 2>&1 \
      || docker build -t agent-fleet-opencode:alpine --build-arg BASE_IMAGE=agent-fleet-base:alpine \
           -f "$AGENTS/opencode/Dockerfile" "$AGENTS/opencode"
    docker image inspect agent-fleet-pi:alpine >/dev/null 2>&1 \
      || docker build -t agent-fleet-pi:alpine --build-arg BASE_IMAGE=agent-fleet-base:alpine \
           -f "$AGENTS/pi/Dockerfile" "$AGENTS/pi"
  fi
fi
echo

set +e
echo "── 1/2 OpenCode ──"
# harness comes from job JSON (opencode); image selected per-task
"$SCRIPTS/run-invoices-example.sh" config/job.123-invoices.opencode.json
OC=$?
OC_ID=$(cat "${FLEET_TMP}/lab-job-id" 2>/dev/null || true)
echo

echo "── 2/2 Pi ──"
# harness comes from job JSON (pi); image selected per-task — not from .env HARNESS
"$SCRIPTS/run-invoices-example.sh" config/job.123-invoices.pi.json
PI=$?
PI_ID=$(cat "${FLEET_TMP}/lab-job-id" 2>/dev/null || true)
set -e

echo
echo "summary  opencode=${OC_ID:-?} $([[ $OC -eq 0 ]] && echo OK || echo FAIL)  |  pi=${PI_ID:-?} $([[ $PI -eq 0 ]] && echo OK || echo FAIL)"
[[ $OC -eq 0 && $PI -eq 0 ]]
