#!/usr/bin/env bash
# The only command that may create a GitLab MR for this repo.
# Runs assert-mr-ready.sh first. Pushing the branch without creating an MR
# is fine (`git push gitlab HEAD`); this script is the create step.
#
#   scripts/open-mr.sh
#   MR_TITLE='feat: …' MR_TARGET=main scripts/open-mr.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"

bash "${ROOT}/scripts/assert-mr-ready.sh"

remote="${MR_REMOTE:-gitlab}"
target="${MR_TARGET:-main}"
title="${MR_TITLE:-}"
branch="$(git rev-parse --abbrev-ref HEAD)"
if [[ "${branch}" == "HEAD" ]]; then
  echo "ERROR: detached HEAD; check out a branch before opening an MR" >&2
  exit 1
fi
if [[ "${branch}" == "${target}" ]]; then
  echo "ERROR: refuse to open an MR from ${branch} onto itself" >&2
  exit 1
fi

# Description cannot contain newlines in git push -o.
desc="Merge bar green on $(bash "${ROOT}/scripts/tree-id.sh"). Do not merge if tests, ship images, or film are still pending — they are not."

args=(
  push "${remote}" "HEAD:refs/heads/${branch}"
  -o merge_request.create
  -o "merge_request.target=${target}"
  -o "merge_request.description=${desc}"
)
if [[ -n "${title}" ]]; then
  args+=(-o "merge_request.title=${title}")
fi

echo "opening MR ${branch} -> ${remote}/${target}"
git "${args[@]}"
