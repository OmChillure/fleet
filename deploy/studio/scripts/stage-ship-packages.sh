#!/usr/bin/env bash
# Stage the worker packages we actually ship (not every OS in --all).
# Rebuild a platform only when host/driver sources changed since that
# platform's last bake, or the bins are missing.
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
    if [[ "${have}" == "${SHA}" ]]; then
      log "worker ${plat}: already ${SHA:0:12}, skip"
      return 0
    fi
    if ! worker_sources_changed "${have}" "${SHA}"; then
      log "worker ${plat}: host/driver unchanged since ${have:0:12}, skip"
      return 0
    fi
  fi
  log "worker ${plat}: building ${flag} for ${SHA:0:12}"
  (
    cd "${WORKDIR}"
    FLEET_WORKER_DIST_DIR="${DIST}" FLEET_GIT_SHA="${SHA}" \
      bash scripts/stage-worker-dist.sh "${flag}"
  )
}

# Common guests. Not --all (no musl / armv7 / windows-arm64).
stage_one linux-x86_64 --bookworm
stage_one linux-aarch64 --aarch64
stage_one windows-x86_64 --windows

log "worker darwin-arm64 / darwin-x86_64: not built here (no macOS cross from Linux)"

python3 "${WORKDIR}/scripts/runner-release.py" check --require-all "${DIST}"

log "shipping worker-dist → ${VPS_HOST}:${VPS_STACK_DIR}/worker-dist"
vps_deploy_ssh "mkdir -p '${VPS_STACK_DIR}/worker-dist'"
rsync -az --delete \
  -e "ssh -i ${VPS_DEPLOY_KEY} -o BatchMode=yes -o IdentitiesOnly=yes ${VPS_SSH_OPTS:-} -o StrictHostKeyChecking=accept-new" \
  "${DIST}/" "${VPS_DEPLOY_USER}@${VPS_HOST}:${VPS_STACK_DIR}/worker-dist/"
printf '%s\n' "${SHA}" > "${STATE_DIR}/worker-dist-sha"
log "worker-dist shipped"
