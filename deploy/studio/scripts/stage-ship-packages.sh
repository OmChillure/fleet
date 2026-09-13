#!/usr/bin/env bash
# Stage the worker packages we actually ship (not every OS in --all).
# Rebuild a platform only when host/driver sources changed *and* the
# staged hashes do not already match [release]. Compiles are not
# bit-identical: rebuilding a locked package fails check. If hashes
# match the lock, keep the bins even when this SHA is new (inbox-only
# deploys). Never skip just because the manifest already says this SHA
# — that hid a lock mismatch after a bad rebuild (ab04bca).
#
#   linux-x86_64   (bookworm glibc — Debian/Ubuntu guests)
#   linux-aarch64  (ARM servers)
#   windows-x86_64 (.exe)
# Skip: musl, armv7, windows-arm64, freebsd.
# darwin-* cannot be cross-compiled on this Linux box.
#
# Usage: stage-ship-packages.sh SHA WORKDIR
# Env: FLEET_WORKER_DIST_DIR (default $STATE_DIR/worker-dist)
set -euo pipefail
# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

SHA="${1:?SHA}"
WORKDIR="${2:?WORKDIR}"
DIST="${FLEET_WORKER_DIST_DIR:-${STATE_DIR}/worker-dist}"
export FLEET_WORKER_DIST_DIR="${DIST}"
export FLEET_GIT_SHA="${SHA}"
mkdir -p "${DIST}"

WORKER_PATHS=(
  crates/fleet-host
  crates/fleet-driver
  crates/agent-fleet-contracts
  runner-packages.toml
  scripts/stage-worker-dist.sh
  scripts/stage-worker-dist-docker.sh
  Cargo.lock
)

manifest_sha() {
  local man="$1/manifest.json"
  [[ -f "${man}" ]] || return 1
  python3 - "${man}" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
s = (m.get("git_sha") or "").strip()
# dirty tree-id is "<40hex>-<suffix>"; compare the commit.
print(s.split("-", 1)[0] if s else "")
PY
}

platform_complete() {
  local d="$1"
  if [[ -f "${d}/fleet-host.exe" && -f "${d}/fleet-driver.exe" ]]; then
    return 0
  fi
  if [[ -f "${d}/fleet-host" && -f "${d}/fleet-driver" ]]; then
    return 0
  fi
  return 1
}

worker_sources_changed() {
  local from="$1"
  local to="$2"
  if [[ -z "${from}" || -z "${to}" ]]; then
    return 0
  fi
  if ! git -C "${REPO}" cat-file -e "${from}^{commit}" 2>/dev/null; then
    return 0
  fi
  if ! git -C "${REPO}" diff --quiet "${from}" "${to}" -- "${WORKER_PATHS[@]}"; then
    return 0
  fi
  return 1
}

stage_one() {
  local plat="$1"
  local flag="$2"
  local dest="${DIST}/${plat}"
  local have
  have="$(manifest_sha "${dest}" || true)"
  if platform_complete "${dest}"; then
    if python3 "${REPO}/scripts/runner-release.py" platform-ok "${DIST}" "${plat}"; then
      log "worker ${plat}: staged hashes match [release], skip"
      return 0
    fi
    if [[ "${have}" == "${SHA}" ]]; then
      log "worker ${plat}: already labeled ${SHA:0:12} but hashes do not match [release] — not rebuilding (not bit-identical). Restore the last registered dist or register these bins."
      return 1
    fi
    if ! worker_sources_changed "${have}" "${SHA}"; then
      log "worker ${plat}: host/driver unchanged since ${have:0:12} but hashes do not match [release] — not rebuilding. Restore registered bins or register."
      return 1
    fi
  fi
  log "worker ${plat}: building ${flag} for ${SHA:0:12}"
  (
    cd "${WORKDIR}"
    FLEET_WORKER_DIST_DIR="${DIST}" FLEET_GIT_SHA="${SHA}" \
      bash scripts/stage-worker-dist.sh "${flag}"
  )
}

ship_dist() {
  log "shipping worker-dist → ${VPS_HOST}:${VPS_STACK_DIR}/worker-dist"
  vps_ssh "mkdir -p '${VPS_STACK_DIR}/worker-dist'"
  rsync -az --delete \
    -e "ssh -i ${VPS_SSH_KEY} -o BatchMode=yes -o IdentitiesOnly=yes ${VPS_SSH_OPTS:-} -o StrictHostKeyChecking=accept-new" \
    "${DIST}/" "${VPS_USER}@${VPS_HOST}:${VPS_STACK_DIR}/worker-dist/"
  printf '%s\n' "${SHA}" > "${STATE_DIR}/worker-dist-sha"
  log "worker-dist shipped"
}

if [[ "${SHIP_ONLY:-0}" == 1 ]]; then
  python3 "${REPO}/scripts/runner-release.py" check --require-all "${DIST}"
  ship_dist
  exit 0
fi

# Common guests. Not --all (no musl / armv7 / windows-arm64).
stage_one linux-x86_64 --bookworm
stage_one linux-aarch64 --aarch64
stage_one windows-x86_64 --windows

log "worker darwin-arm64 / darwin-x86_64: not built here (no macOS cross from Linux)"

python3 "${REPO}/scripts/runner-release.py" check --require-all "${DIST}"

if [[ "${STAGE_ONLY:-0}" == 1 ]]; then
  log "worker-dist staged locally (${DIST}); not shipping until plane is healthy"
  exit 0
fi

ship_dist
