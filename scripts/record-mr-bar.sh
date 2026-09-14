#!/usr/bin/env bash
# Record one merge-bar receipt for the current tree-id.
# Stamps live under .tmp/mr-bar/<tree-id>/ (gitignored). A new commit or
# dirty worktree changes tree-id and invalidates every stamp.
#
#   scripts/record-mr-bar.sh tests
#   scripts/record-mr-bar.sh images
#   scripts/record-mr-bar.sh film
#   scripts/record-mr-bar.sh small-tweak "typo in comment"
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

usage() {
  echo "usage: scripts/record-mr-bar.sh tests|images|film|small-tweak [reason]" >&2
  exit 2
}

kind="${1:-}"
[[ -n "${kind}" ]] || usage
shift || true

tree_id="$(bash "${ROOT}/scripts/tree-id.sh")"
dir="${ROOT}/.tmp/mr-bar/${tree_id}"
mkdir -p "${dir}"

image_sha() {
  docker image inspect -f '{{index .Config.Labels "fleet.git_sha"}}' "$1" 2>/dev/null || true
}

have_ship_image() {
  local name="$1" tag have
  for tag in "${tree_id}" local; do
    if docker image inspect "${name}:${tag}" >/dev/null 2>&1; then
      have="$(image_sha "${name}:${tag}")"
      if [[ "${have}" == "${tree_id}" ]]; then
        echo "${name}:${tag}"
        return 0
      fi
    fi
  done
  return 1
}

film_review_filled() {
  local scope="${ROOT}/HUMAN_TEST_SCOPE.md" recorded frames
  [[ -f "${scope}" ]] || return 1
  recorded="$(sed -n 's/^- Recorded on final merge tree:[[:space:]]*//p' "${scope}" | head -1)"
  frames="$(sed -n 's/^- Frames + webm inspected:[[:space:]]*//p' "${scope}" | head -1)"
  [[ -n "${recorded}" && -n "${frames}" ]]
}

case "${kind}" in
  tests)
    {
      echo "tree_id=${tree_id}"
      echo "recorded_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "attest=just test && just test-integration were green on this tree-id"
    } >"${dir}/tests"
    echo "recorded tests for ${tree_id}"
    ;;
  images)
    api="$(have_ship_image agent-fleet-api)" || {
      echo "ERROR: no agent-fleet-api image with fleet.git_sha=${tree_id}" >&2
      echo "       docker build --build-arg FLEET_GIT_SHA=${tree_id} -t agent-fleet-api:${tree_id} -f Dockerfile ." >&2
      exit 1
    }
    ui="$(have_ship_image agent-fleet-ui)" || {
      echo "ERROR: no agent-fleet-ui image with fleet.git_sha=${tree_id}" >&2
      echo "       docker build --build-arg FLEET_GIT_SHA=${tree_id} -t agent-fleet-ui:${tree_id} -f Dockerfile.ui ." >&2
      exit 1
    }
    {
      echo "tree_id=${tree_id}"
      echo "recorded_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "api=${api}"
      echo "ui=${ui}"
    } >"${dir}/images"
    echo "recorded images for ${tree_id} (${api}, ${ui})"
    ;;
  film)
    if ! film_review_filled; then
      echo "ERROR: HUMAN_TEST_SCOPE.md Film review is empty." >&2
      echo "       Film the final tree, inspect frames + .webm, fill the two fields." >&2
      exit 1
    fi
    {
      echo "tree_id=${tree_id}"
      echo "recorded_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    } >"${dir}/film"
    echo "recorded film for ${tree_id}"
    ;;
  small-tweak)
    reason="${*:-}"
    if [[ ${#reason} -lt 8 ]]; then
      echo "ERROR: small-tweak needs a reason (typo / comment nit / import order / one-line docs)." >&2
      exit 1
    fi
    {
      echo "tree_id=${tree_id}"
      echo "recorded_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)"
      echo "reason=${reason}"
    } >"${dir}/small-tweak"
    echo "recorded small-tweak for ${tree_id}"
    ;;
  *)
    usage
    ;;
esac
