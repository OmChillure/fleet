#!/usr/bin/env bash
# Identity of *this worktree* for live stacks.
#
#   clean  → <git rev-parse HEAD>
#   dirty  → <HEAD>-<12 hex of worktree contents vs HEAD>
#
# Install, `run-every-test.sh --prepare-kind`, `fleet-dev` / host `dx` bake this
# into /health.git_sha and UI /fleet-git-sha.txt. stack_identity.py requires
# a match. Editing files after that bake makes verification invalid until you
# run the install script or rebuild (prepare-kind + host dx).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"
head="$(git rev-parse HEAD)"
# dx rewrites this tracked file on every host build — not a source edit.
# Identity is source; generated CSS must not invalidate a just-baked stack.
PATHSPEC=(
  --
  .
  ':!crates/fleet-ui/assets/tailwind.css'
)
untracked="$(
  git ls-files --others --exclude-standard \
    | grep -Ev '(^|/)__pycache__/|\.pyc$|\.pyo$' || true
)"
if git diff --quiet HEAD "${PATHSPEC[@]}" && [[ -z "${untracked}" ]]; then
  printf '%s\n' "${head}"
  exit 0
fi
fp="$(
  {
    git diff HEAD "${PATHSPEC[@]}"
    printf '%s\n' "${untracked}" | LC_ALL=C sort | while IFS= read -r f; do
      [[ -n "${f}" && -f "${f}" ]] || continue
      git hash-object "${f}"
    done
  } | git hash-object --stdin
)"
printf '%s-%s\n' "${head}" "${fp:0:12}"
