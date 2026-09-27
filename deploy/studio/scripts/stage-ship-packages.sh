#!/usr/bin/env bash
# Stage the worker packages we actually ship (not every OS in --all).
# Rebuild a platform only when host/driver sources changed since the
# staged manifest. Compiles are not bit-identical, so an unchanged
# source tree keeps the bins already staged even when git [release]
# hashes lag (the deployer registers those hashes; the author only
# bumps versions and per-OS revs). If hashes match [release], keep the
# bins. Never rebuild just because the manifest SHA is old.
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

REBUILT=0
KEPT_STALE=0

stage_one() {
  local plat="$1"
  local flag="$2"
  local dest="${DIST}/${plat}"
  local have
  have="$(manifest_sha "${dest}" || true)"
  if platform_complete "${dest}"; then
    if python3 "${WORKDIR}/scripts/runner-release.py" platform-ok "${DIST}" "${plat}"; then
      log "worker ${plat}: staged hashes match [release], skip"
      return 0
    fi
    if ! worker_sources_changed "${have}" "${SHA}"; then
      log "worker ${plat}: host/driver unchanged since ${have:0:12}; keeping staged bins"
      KEPT_STALE=1
      return 0
    fi
  fi
  log "worker ${plat}: building ${flag} for ${SHA:0:12}"
  REBUILT=1
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

apply_followup_lock() {
  local followup="${STATE_DIR}/runner-packages.followup.toml"
  if [[ ! -f "${followup}" ]]; then
    log "staged bins do not match git [release] and ${followup} is missing"
    return 1
  fi
  cp "${followup}" "${WORKDIR}/runner-packages.toml"
  log "checking staged bins against the registered follow-up lock"
}

lock_matches_git() {
  python3 "${WORKDIR}/scripts/runner-release.py" platform-ok "${DIST}" linux-x86_64 \
    && python3 "${WORKDIR}/scripts/runner-release.py" platform-ok "${DIST}" linux-aarch64 \
    && python3 "${WORKDIR}/scripts/runner-release.py" platform-ok "${DIST}" windows-x86_64
}

if [[ "${SHIP_ONLY:-0}" == 1 ]]; then
  if ! lock_matches_git; then
    apply_followup_lock
  fi
  python3 "${WORKDIR}/scripts/runner-release.py" check --require-all "${DIST}"
  ship_dist
  exit 0
fi

# Common guests. Not --all (no musl / armv7 / windows-arm64).
stage_one linux-x86_64 --bookworm
stage_one linux-aarch64 --aarch64
stage_one windows-x86_64 --windows

log "worker darwin-arm64 / darwin-x86_64: not built here (no macOS cross from Linux)"

# Register with the deploy checkout's script. The archive copy does not
# know about --deployer-revs until that commit is what we are building.
cp "${REPO}/scripts/runner-release.py" "${WORKDIR}/scripts/runner-release.py"

if [[ "${REBUILT}" == 1 ]]; then
  baseline="$(mktemp)"
  followup="${STATE_DIR}/runner-packages.followup.toml"
  if [[ -f "${followup}" ]]; then
    cp "${followup}" "${baseline}"
  elif git -C "${REPO}" cat-file -e "${SHA}^:runner-packages.toml" 2>/dev/null; then
    git -C "${REPO}" show "${SHA}^:runner-packages.toml" > "${baseline}"
  else
    cp "${WORKDIR}/runner-packages.toml" "${baseline}"
  fi
  python3 "${WORKDIR}/scripts/runner-release.py" register --deployer-revs \
    --baseline "${baseline}" "${DIST}"
  rm -f "${baseline}"
  cp "${WORKDIR}/runner-packages.toml" "${STATE_DIR}/runner-packages.followup.toml"
  printf '%s\n' "${SHA}" > "${STATE_DIR}/runner-release-followup"
  log "registered runner hashes (${STATE_DIR}/runner-packages.followup.toml)"
elif [[ "${KEPT_STALE}" == 1 ]]; then
  baseline="$(mktemp)"
  followup="${STATE_DIR}/runner-packages.followup.toml"
  if [[ -f "${followup}" ]]; then
    cp "${followup}" "${baseline}"
  elif git -C "${REPO}" cat-file -e "${SHA}^:runner-packages.toml" 2>/dev/null; then
    git -C "${REPO}" show "${SHA}^:runner-packages.toml" > "${baseline}"
  else
    cp "${WORKDIR}/runner-packages.toml" "${baseline}"
  fi
  python3 "${WORKDIR}/scripts/runner-release.py" register --deployer-revs \
    --baseline "${baseline}" "${DIST}"
  rm -f "${baseline}"
  cp "${WORKDIR}/runner-packages.toml" "${STATE_DIR}/runner-packages.followup.toml"
  printf '%s\n' "${SHA}" > "${STATE_DIR}/runner-release-followup"
  log "registered kept bins; bumped any rev the author missed"
fi

python3 "${WORKDIR}/scripts/runner-release.py" check --require-all "${DIST}"

if [[ "${STAGE_ONLY:-0}" == 1 ]]; then
  log "worker-dist staged locally (${DIST}); not shipping until plane is healthy"
  exit 0
fi

ship_dist
