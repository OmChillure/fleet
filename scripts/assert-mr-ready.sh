#!/usr/bin/env bash
# Exit 0 only when this worktree may create a GitLab MR.
# Receipts: scripts/record-mr-bar.sh after each bar item on *this* tree-id.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

tree_id="$(bash "${ROOT}/scripts/tree-id.sh")"
dir="${ROOT}/.tmp/mr-bar/${tree_id}"
fail=0

need() {
  local name="$1"
  if [[ ! -f "${dir}/${name}" ]]; then
    echo "FAIL  missing ${dir}/${name}" >&2
    fail=1
    return 1
  fi
  echo "PASS  ${name} ($(head -1 "${dir}/${name}"))"
}

echo "tree-id ${tree_id}"
need tests || true
need images || true
if [[ -f "${dir}/small-tweak" ]]; then
  echo "PASS  small-tweak ($(grep '^reason=' "${dir}/small-tweak" || true))"
else
  need film || true
fi

if (( fail )); then
  echo >&2
  echo "Merge bar is not green on ${tree_id}." >&2
  echo "Do not create an MR. A branch may exist; the MR must not." >&2
  echo "Record receipts after each item:" >&2
  echo "  just test && just test-integration && scripts/record-mr-bar.sh tests" >&2
  echo "  docker build --build-arg FLEET_GIT_SHA=${tree_id} -t agent-fleet-api:${tree_id} -f Dockerfile ." >&2
  echo "  docker build --build-arg FLEET_GIT_SHA=${tree_id} -t agent-fleet-ui:${tree_id} -f Dockerfile.ui ." >&2
  echo "  scripts/record-mr-bar.sh images" >&2
  echo "  # film HUMAN_TEST_SCOPE, fill Film review, then:" >&2
  echo "  scripts/record-mr-bar.sh film" >&2
  echo "  # or, only for a typo/comment nit:" >&2
  echo "  scripts/record-mr-bar.sh small-tweak \"reason\"" >&2
  echo "Then: scripts/open-mr.sh" >&2
  exit 1
fi

echo "assert-mr-ready ok"
