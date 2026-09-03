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
#
# Constraints (fail-closed):
#   - Identity is git HEAD + worktree fingerprint only.
#   - FLEET_GIT_SHA is a bake *input* callers set FROM this script. Never the reverse.
#   - Generated crates/fleet-ui/assets/tailwind.css is not source (dx rewrites it).
#   - Untracked source (not gitignored, not __pycache__) must dirty the id.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"
# Hooks / leftover bake env must not redirect this worktree's identity.
unset FLEET_GIT_SHA GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_COMMON_DIR

self_test() {
  local tmp script head clean dirty1 dirty2 id fake
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/tree-id-self-test.XXXXXX")"
  # Expand now: tmp is local and gone when the EXIT trap runs after return.
  # EXIT, not RETURN: a RETURN trap would fire on every helper return.
  trap "rm -rf $(printf '%q' "${tmp}")" EXIT
  git init -q "${tmp}"
  git -C "${tmp}" config user.email "tree-id@example.test"
  git -C "${tmp}" config user.name "tree-id"
  mkdir -p "${tmp}/scripts" "${tmp}/crates/fleet-ui/assets"
  script="${tmp}/scripts/tree-id.sh"
  cp "${ROOT}/scripts/tree-id.sh" "${script}"
  chmod +x "${script}"
  printf 'one\n' >"${tmp}/tracked.txt"
  printf 'generated\n' >"${tmp}/crates/fleet-ui/assets/tailwind.css"
  git -C "${tmp}" add scripts/tree-id.sh tracked.txt crates/fleet-ui/assets/tailwind.css
  git -C "${tmp}" commit -q -m init
  head="$(git -C "${tmp}" rev-parse HEAD)"
  fake="ffffffffffffffffffffffffffffffffffffffff"

  clean="$(env FLEET_GIT_SHA="${fake}" bash "${script}")"
  [[ "${clean}" == "${head}" ]] || {
    echo "FAIL: clean must be HEAD, got ${clean}" >&2
    return 1
  }
  [[ "${clean}" =~ ^[0-9a-f]{40}$ ]] || {
    echo "FAIL: clean format ${clean}" >&2
    return 1
  }
  [[ "${clean}" != "${fake}" ]] || {
    echo "FAIL: used FLEET_GIT_SHA env as the id" >&2
    return 1
  }

  printf 'two\n' >"${tmp}/tracked.txt"
  dirty1="$(env FLEET_GIT_SHA="${fake}" bash "${script}")"
  [[ "${dirty1}" =~ ^[0-9a-f]{40}-[0-9a-f]{12}$ ]] || {
    echo "FAIL: dirty format ${dirty1}" >&2
    return 1
  }
  [[ "${dirty1}" == "${head}-"* ]] || {
    echo "FAIL: dirty must be HEAD-<12hex>, got ${dirty1}" >&2
    return 1
  }

  printf 'three\n' >"${tmp}/tracked.txt"
  dirty2="$(env FLEET_GIT_SHA="${fake}" bash "${script}")"
  [[ "${dirty1}" != "${dirty2}" ]] || {
    echo "FAIL: suffix must change when contents change" >&2
    return 1
  }

  printf 'one\n' >"${tmp}/tracked.txt"
  [[ "$(env FLEET_GIT_SHA="${fake}" bash "${script}")" == "${head}" ]] || {
    echo "FAIL: restore tracked file must be clean" >&2
    return 1
  }

  printf 'rewritten by dx\n' >"${tmp}/crates/fleet-ui/assets/tailwind.css"
  [[ "$(env FLEET_GIT_SHA="${fake}" bash "${script}")" == "${head}" ]] || {
    echo "FAIL: generated tailwind.css must not dirty the id" >&2
    return 1
  }

  mkdir -p "${tmp}/crates/fleet-ui"
  printf 'backend = "http://127.0.0.1:18280/api/"\n' >"${tmp}/crates/fleet-ui/Dioxus.toml"
  [[ "$(env FLEET_GIT_SHA="${fake}" bash "${script}")" == "${head}" ]] || {
    echo "FAIL: Dioxus.toml proxy rewrite must not dirty the id" >&2
    return 1
  }

  printf 'new\n' >"${tmp}/untracked.rs"
  id="$(env FLEET_GIT_SHA="${fake}" bash "${script}")"
  [[ "${id}" =~ ^[0-9a-f]{40}-[0-9a-f]{12}$ ]] || {
    echo "FAIL: untracked source must dirty the id, got ${id}" >&2
    return 1
  }

  rm -f "${tmp}/untracked.rs"
  [[ "$(env FLEET_GIT_SHA="${fake}" bash "${script}")" == "${head}" ]] || {
    echo "FAIL: removing untracked source must be clean" >&2
    return 1
  }

  printf 'tree-id self-test ok\n'
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit 0
fi

head="$(git rev-parse HEAD)"
# dx rewrites this tracked file on every host build — not a source edit.
# Identity is source; generated CSS must not invalidate a just-baked stack.
PATHSPEC=(
  --
  .
  ':!crates/fleet-ui/assets/tailwind.css'
  # Generated/reviewed per MR; filling Film review must not invalidate a bake.
  ':!HUMAN_TEST_SCOPE.md'
  # fleet-dev rewrites the proxy port when Kind owns :8080.
  ':!crates/fleet-ui/Dioxus.toml'
)
untracked="$(
  git ls-files --others --exclude-standard "${PATHSPEC[@]}" \
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
