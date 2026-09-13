#!/usr/bin/env bash
# Fetch GitLab main on this PC, build images, ship them to the VPS, compose up.
# The VPS never talks to GitLab.
#
#   ./scripts/deploy.sh           # deploy origin/main if SHA changed
#   FORCE=1 ./scripts/deploy.sh   # rebuild and ship even if SHA matches

# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

FORCE="${FORCE:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"
# origin = git archive of origin/main. worktree = this checkout (for unmerged fixes).
DEPLOY_SOURCE="${DEPLOY_SOURCE:-origin}"
API_IMAGE=agent-fleet-api
UI_IMAGE=agent-fleet-ui
export GIT_SSH_COMMAND="${GIT_SSH_COMMAND:-ssh -o BatchMode=yes -o IdentitiesOnly=yes -i ${HOME}/.ssh/id_ed25519}"

mkdir -p "${STATE_DIR}"
exec 9>"${STATE_DIR}/deploy.lock"
if ! flock -n 9; then
  log "another deploy is running; exiting"
  exit 0
fi

CTL="${STATE_DIR}/ssh-ctl"
rm -f "${CTL}"
VPS_SSH_OPTS="-o ControlMaster=auto -o ControlPath=${CTL} -o ControlPersist=60"
cleanup() {
  ssh -i "${VPS_DEPLOY_KEY}" -o ControlPath="${CTL}" -O exit "${VPS_DEPLOY_USER}@${VPS_HOST}" >/dev/null 2>&1 || true
  rm -f "${CTL}"
  if [[ -n "${WORKDIR:-}" && -d "${WORKDIR}" ]]; then
    rm -rf "${WORKDIR}"
  fi
}
trap cleanup EXIT

compose_up() {
  local sha="$1"
  vps_deploy_scp "${ROOT}/compose.yml" "${VPS_DEPLOY_USER}@${VPS_HOST}:${VPS_STACK_DIR}/compose.yml"
  vps_deploy_scp "${ROOT}/Caddyfile" "${VPS_DEPLOY_USER}@${VPS_HOST}:${VPS_STACK_DIR}/Caddyfile"
  vps_deploy_ssh "sudo /usr/local/sbin/fleet-stack up '${sha}'"
}

wait_healthy() {
  local sha="$1"
  local deadline=$((SECONDS + HEALTH_TIMEOUT_SECS))
  local health ui_sha
  while (( SECONDS < deadline )); do
    health="$(vps_deploy_ssh "curl -fsS --max-time 5 http://127.0.0.1:3030/health" 2>/dev/null || true)"
    ui_sha="$(vps_deploy_ssh "curl -fsS --max-time 5 http://127.0.0.1:3030/fleet-git-sha.txt" 2>/dev/null | tr -d '[:space:]' || true)"
    if [[ -n "${health}" && -n "${ui_sha}" ]] \
      && python3 -c 'import json,sys
want, ui, raw = sys.argv[1], sys.argv[2], sys.argv[3]
body = json.loads(raw)
got = (body.get("git_sha") or "").strip()
sys.exit(0 if body.get("status")=="ok" and got==want and ui==want else 1)' \
        "${sha}" "${ui_sha}" "${health}"
    then
      log "healthy at ${PUBLIC_BASE} git_sha=${sha}"
      return 0
    fi
    sleep 5
  done
  log "health check failed for ${sha}"
  return 1
}

prune_old_images() {
  local keep_a="$1"
  local keep_b="$2"
  vps_deploy_ssh "KEEP_A='${keep_a}' KEEP_B='${keep_b}' bash -s" <<'EOF'
set -euo pipefail
sudo /usr/local/sbin/fleet-stack images | while read -r ref; do
  [[ -n "${ref}" && "${ref}" != *":<none>" ]] || continue
  tag="${ref##*:}"
  [[ "${tag}" == "${KEEP_A}" || "${tag}" == "${KEEP_B}" ]] && continue
  sudo /usr/local/sbin/fleet-stack rmi "${ref}" >/dev/null || true
done
EOF
}

if [[ "${DEPLOY_SOURCE}" == "worktree" ]]; then
  SHA="$(bash "${REPO}/scripts/tree-id.sh")"
  log "deploying local worktree ${SHA}"
else
  log "fetching ${GIT_REMOTE}/${GIT_BRANCH} in ${REPO}"
  git -C "${REPO}" fetch --quiet "${GIT_REMOTE}" "${GIT_BRANCH}"
  SHA="$(git -C "${REPO}" rev-parse "${GIT_REMOTE}/${GIT_BRANCH}")"
fi
SHORT="${SHA:0:12}"
fail_notify() {
  notify_desktop critical "Agent Fleet deploy failed" "${SHORT}: $*"
}

LAST_LOCAL="$(cat "${STATE_DIR}/deployed-sha" 2>/dev/null || true)"
LAST_REMOTE="$(vps_deploy_ssh "cat '${VPS_STACK_DIR}/DEPLOYED_SHA' 2>/dev/null || true")"

if [[ "${FORCE}" != 1 && "${SHA}" == "${LAST_LOCAL}" && "${SHA}" == "${LAST_REMOTE}" ]]; then
  log "already deployed ${SHORT}; nothing to do"
  exit 0
fi

# A leftover worktree id (sha-sha) is not origin/main. Keep a patch by
# leaving the timer off; once the timer is on, origin/main is the target.
if [[ "${DEPLOY_SOURCE}" != "worktree" && "${LAST_REMOTE}" == *-* ]]; then
  log "replacing patched deploy ${LAST_REMOTE:0:20}… with origin ${SHORT}"
fi

log "deploying ${SHORT}"

if [[ "${SKIP_BUILD}" != 1 ]]; then
  WORKDIR="$(mktemp -d "${ROOT}/.build.XXXXXX")"
  log "materializing ${SHORT} into ${WORKDIR}"
  if [[ "${DEPLOY_SOURCE}" == "worktree" ]]; then
    rsync -a --delete \
      --exclude target --exclude .git --exclude data --exclude tmp --exclude .tmp \
      --exclude node_modules --exclude '*.pyc' --exclude __pycache__ \
      "${REPO}/" "${WORKDIR}/"
  else
    git -C "${REPO}" archive "${SHA}" | tar -x -C "${WORKDIR}"
    # origin/main Dockerfile omitted this until the tree includes it; include_str! needs it.
    if [[ -f "${WORKDIR}/runner-packages.toml" ]] \
      && ! grep -q 'COPY runner-packages.toml' "${WORKDIR}/Dockerfile"; then
      sed -i '/COPY crates \.\/crates/a COPY runner-packages.toml ./' "${WORKDIR}/Dockerfile"
      log "patched Dockerfile to COPY runner-packages.toml"
    fi
  fi

  log "building ${API_IMAGE}:${SHORT} and ${UI_IMAGE}:${SHORT}"
  fail=0
  docker build --build-arg "FLEET_GIT_SHA=${SHA}" \
    -f "${WORKDIR}/Dockerfile" \
    -t "${API_IMAGE}:${SHA}" \
    "${WORKDIR}" &
  api_pid=$!
  docker build --build-arg "FLEET_GIT_SHA=${SHA}" \
    -f "${WORKDIR}/Dockerfile.ui" \
    -t "${UI_IMAGE}:${SHA}" \
    "${WORKDIR}" &
  ui_pid=$!
  wait "${api_pid}" || fail=1
  wait "${ui_pid}" || fail=1
  if [[ "${fail}" -ne 0 ]]; then
    log "docker build failed"
    fail_notify "docker build failed"
    exit 1
  fi
fi

if ! docker image inspect "${API_IMAGE}:${SHA}" >/dev/null 2>&1 \
  || ! docker image inspect "${UI_IMAGE}:${SHA}" >/dev/null 2>&1; then
  log "images for ${SHORT} are not present locally (build failed or SKIP_BUILD=1)"
  fail_notify "images missing locally"
  exit 1
fi

# Host/driver packages for install + auto-update. Rebuild a platform only
# when those crates changed. After a failed UI build we never get here.
STAGE_SRC="${WORKDIR:-${REPO}}"
if [[ -z "${WORKDIR:-}" ]]; then
  STAGE_SRC="$(mktemp -d "${ROOT}/.build.XXXXXX")"
  git -C "${REPO}" archive "${SHA}" | tar -x -C "${STAGE_SRC}"
fi
if ! bash "${ROOT}/scripts/stage-ship-packages.sh" "${SHA}" "${STAGE_SRC}"; then
  fail_notify "worker package stage failed"
  exit 1
fi
if [[ -z "${WORKDIR:-}" ]]; then
  rm -rf "${STAGE_SRC}"
fi

log "shipping images to ${VPS_HOST}"
docker save "${API_IMAGE}:${SHA}" "${UI_IMAGE}:${SHA}" \
  | zstd -T0 \
  | vps_deploy_ssh "zstd -d | sudo /usr/local/sbin/fleet-stack load"

PREV="${LAST_REMOTE}"
if ! compose_up "${SHA}"; then
  log "compose up failed for ${SHORT}"
fi

if wait_healthy "${SHA}"; then
  vps_deploy_ssh "printf '%s\n' '${SHA}' > '${VPS_STACK_DIR}/DEPLOYED_SHA' && printf '%s\n' '${PREV}' > '${VPS_STACK_DIR}/PREVIOUS_SHA'"
  printf '%s\n' "${SHA}" > "${STATE_DIR}/deployed-sha"
  prune_old_images "${SHA}" "${PREV}"
  log "deployed ${SHORT} → ${PUBLIC_BASE}"
  notify_desktop normal "Agent Fleet deployed" \
    "${SHORT} is live at ${PUBLIC_BASE}"
  exit 0
fi

if [[ -n "${PREV}" && "${PREV}" != "${SHA}" ]]; then
  log "rolling back to ${PREV:0:12}"
  compose_up "${PREV}"
  if wait_healthy "${PREV}"; then
    vps_deploy_ssh "printf '%s\n' '${PREV}' > '${VPS_STACK_DIR}/DEPLOYED_SHA'"
    printf '%s\n' "${PREV}" > "${STATE_DIR}/deployed-sha"
    log "rollback to ${PREV:0:12} succeeded"
    fail_notify "health failed; rolled back to ${PREV:0:12}"
    exit 1
  fi
  log "rollback health check also failed"
  fail_notify "health and rollback of ${PREV:0:12} both failed"
  exit 1
fi

fail_notify "did not become healthy (no previous SHA to roll back)"
exit 1
