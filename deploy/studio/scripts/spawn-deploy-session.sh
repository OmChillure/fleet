#!/usr/bin/env bash
# Fetch GitLab main. If the VPS is not already on that SHA, spawn (or resume)
# a Grok session to deploy it. Does not ship images itself — the session runs
# deploy.sh. Uses a *session* lock so deploy.sh can still take deploy.lock.
#
#   ./scripts/spawn-deploy-session.sh           # skip or spawn
#   FORCE=1 ./scripts/spawn-deploy-session.sh   # spawn even if already live
#   DRY_RUN=1 ./scripts/spawn-deploy-session.sh # print the decision and exit

# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

FORCE="${FORCE:-0}"
DRY_RUN="${DRY_RUN:-0}"
GROK="${GROK:-${HOME}/.grok/bin/grok}"
export PATH="${HOME}/.grok/bin:${HOME}/.local/bin:/usr/bin:/bin:${PATH:-}"
export GIT_SSH_COMMAND="${GIT_SSH_COMMAND:-ssh -o BatchMode=yes -o IdentitiesOnly=yes -i ${HOME}/.ssh/id_ed25519}"

mkdir -p "${STATE_DIR}/logs"

# Separate from deploy.sh's deploy.lock — if we held that, deploy.sh would
# no-op with "another deploy is running".
exec 8>"${STATE_DIR}/session.lock"
if ! flock -n 8; then
  log "another grok deploy session is running; exiting"
  exit 0
fi

read_state() {
  local f="$1"
  [[ -f "${STATE_DIR}/${f}" ]] || return 0
  tr -d '[:space:]' <"${STATE_DIR}/${f}"
}

live_matches() {
  local want="$1" health ui
  health="$(curl -fsS --max-time 8 "${PUBLIC_BASE}/health" 2>/dev/null || true)"
  ui="$(curl -fsS --max-time 8 "${PUBLIC_BASE}/fleet-git-sha.txt" 2>/dev/null | tr -d '[:space:]' || true)"
  if [[ -z "${health}" || -z "${ui}" ]]; then
    return 2
  fi
  python3 -c 'import json,sys
want, ui, raw = sys.argv[1], sys.argv[2], sys.argv[3]
body = json.loads(raw)
got = (body.get("git_sha") or "").strip()
sys.exit(0 if body.get("status")=="ok" and got==want and ui==want else 1)' \
    "${want}" "${ui}" "${health}"
}

if [[ ! -x "${GROK}" ]]; then
  log "ABORT: grok not executable at ${GROK}"
  exit 1
fi
if [[ ! -f "${HOME}/.grok/auth.json" && -z "${XAI_API_KEY:-}" ]]; then
  log "ABORT: no Grok login (${HOME}/.grok/auth.json) and XAI_API_KEY unset"
  exit 1
fi

log "fetching ${GIT_REMOTE}/${GIT_BRANCH} in ${REPO}"
git -C "${REPO}" fetch --quiet "${GIT_REMOTE}" "${GIT_BRANCH}"
SHA="$(git -C "${REPO}" rev-parse "${GIT_REMOTE}/${GIT_BRANCH}")"
SHORT="${SHA:0:12}"

LAST="$(read_state deployed-sha)"
STATUS="$(read_state grok-status)"
STATUS_SHA="$(read_state grok-sha)"
SESSION="$(read_state grok-session)"

ACTION=new
REASON=""

if [[ "${FORCE}" == 1 ]]; then
  ACTION=new
  REASON="FORCE=1"
  SESSION=""
elif [[ -n "${LAST}" && "${SHA}" == "${LAST}" ]]; then
  if live_matches "${SHA}"; then
    ACTION=skip
    REASON="already live at ${PUBLIC_BASE} git_sha=${SHORT}"
  else
    rc=$?
    if [[ "${rc}" -eq 2 ]]; then
      ACTION=skip
      REASON="SHA ${SHORT} already recorded; live health unreachable this tick"
    else
      ACTION=new
      REASON="recorded ${SHORT} but live identity does not match; repair"
      SESSION=""
    fi
  fi
elif [[ "${STATUS}" == "blocked" && "${STATUS_SHA}" == "${SHA}" ]]; then
  ACTION=skip
  REASON="grok marked ${SHORT} blocked; FORCE=1 to retry"
elif [[ -n "${SESSION}" && "${STATUS_SHA}" == "${SHA}" && "${STATUS}" != "deployed" ]]; then
  ACTION=resume
  REASON="resume session ${SESSION} for ${SHORT} (status=${STATUS:-unset})"
else
  ACTION=new
  REASON="origin/main ${SHORT} is not deployed (last=${LAST:-none})"
  SESSION=""
fi

log "${ACTION}: ${REASON}"

if [[ "${ACTION}" == "skip" ]]; then
  exit 0
fi

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
TICK_PROMPT="${STATE_DIR}/logs/prompt-${STAMP}.md"
{
  cat <<EOF
# This tick

- time: ${STAMP}
- origin/main: \`${SHA}\`
- last deployed-sha: \`${LAST:-none}\`
- previous grok-status: \`${STATUS:-none}\` for \`${STATUS_SHA:-none}\`
- action: **${ACTION}** (${REASON})
- public: ${PUBLIC_BASE}
- state dir: \`${STATE_DIR}\`
- ship: \`${ROOT}/scripts/deploy.sh\`
- product: \`${REPO}\`
- VPS: \`${VPS_DEPLOY_USER}@${VPS_HOST}\`

Write \`grok-status\` and \`grok-report\` under the state dir before you stop.

---

EOF
  cat "${ROOT}/PROMPT.md"
} >"${TICK_PROMPT}"

if [[ "${DRY_RUN}" == 1 ]]; then
  log "DRY_RUN: would ${ACTION} grok for ${SHORT} (session=${SESSION:-new})"
  log "DRY_RUN: prompt at ${TICK_PROMPT}"
  exit 0
fi

printf '%s\n' "${SHA}" >"${STATE_DIR}/grok-sha"
printf 'running\n' >"${STATE_DIR}/grok-status"

GROK_JSON="${STATE_DIR}/logs/grok-${STAMP}.json"
GROK_ERR="${STATE_DIR}/logs/grok-${STAMP}.err"
export GROK_LOG_FILE="${STATE_DIR}/logs/grok-${STAMP}.log"

RULES="Do not git checkout, reset, or clean ${REPO}. Do not rotate pair tokens or force a re-pair. Deploy only via ${ROOT}/scripts/deploy.sh. Production SHA must be a GitLab commit. Write grok-status (deployed|blocked|failed) and grok-report before stopping."

cmd=(
  "${GROK}"
  --cwd "${ROOT}"
  --always-approve
  --no-auto-update
  --output-format json
  --rules "${RULES}"
  --prompt-file "${TICK_PROMPT}"
)
if [[ "${ACTION}" == "resume" ]]; then
  cmd+=(--resume "${SESSION}")
fi

log "starting grok ${ACTION} for ${SHORT}"
set +e
"${cmd[@]}" >"${GROK_JSON}" 2>"${GROK_ERR}"
rc=$?
set -e

NEW_SESSION="$(python3 -c 'import json,sys
p=sys.argv[1]
try:
    print((json.load(open(p)).get("sessionId") or "").strip())
except Exception:
    print("")' "${GROK_JSON}")"
if [[ -n "${NEW_SESSION}" ]]; then
  printf '%s\n' "${NEW_SESSION}" >"${STATE_DIR}/grok-session"
  log "grok session ${NEW_SESSION}"
fi

NOW_STATUS="$(read_state grok-status)"
NOW_DEPLOYED="$(read_state deployed-sha)"
if [[ "${NOW_DEPLOYED}" == "${SHA}" ]]; then
  printf 'deployed\n' >"${STATE_DIR}/grok-status"
elif [[ "${NOW_STATUS}" == "running" ]]; then
  printf 'failed\n' >"${STATE_DIR}/grok-status"
fi

log "grok exit ${rc} status=$(read_state grok-status) deployed=$(read_state deployed-sha)"
if [[ "${rc}" -ne 0 ]]; then
  tail -n 40 "${GROK_ERR}" >&2 || true
fi
exit "${rc}"
