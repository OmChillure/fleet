# shellcheck shell=bash
# Shared helpers. Source from other scripts; do not run.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=../config.sh
source "${ROOT}/config.sh"

VPS_SSH_KEY="${VPS_SSH_KEY/#\~/${HOME}}"
VPS_DEPLOY_KEY="${VPS_DEPLOY_KEY/#\~/${HOME}}"

vps_ssh() {
  ssh -i "${VPS_SSH_KEY}" \
    -o BatchMode=yes \
    -o IdentitiesOnly=yes \
    -o StrictHostKeyChecking=accept-new \
    ${VPS_SSH_OPTS:-} \
    "${VPS_USER}@${VPS_HOST}" "$@"
}

vps_scp() {
  scp -i "${VPS_SSH_KEY}" \
    -o BatchMode=yes \
    -o IdentitiesOnly=yes \
    -o StrictHostKeyChecking=accept-new \
    ${VPS_SSH_OPTS:-} \
    "$@"
}

vps_deploy_ssh() {
  ssh -i "${VPS_DEPLOY_KEY}" \
    -o BatchMode=yes \
    -o IdentitiesOnly=yes \
    -o StrictHostKeyChecking=accept-new \
    ${VPS_SSH_OPTS:-} \
    "${VPS_DEPLOY_USER}@${VPS_HOST}" "$@"
}

vps_deploy_scp() {
  scp -i "${VPS_DEPLOY_KEY}" \
    -o BatchMode=yes \
    -o IdentitiesOnly=yes \
    -o StrictHostKeyChecking=accept-new \
    ${VPS_SSH_OPTS:-} \
    "$@"
}

log() { printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"; }

# User-session desktop notification (this PC). Stays until dismissed.
# Harmless if the session is gone.
notify_desktop() {
  local urgency="${1:?}"
  local summary="${2:?}"
  local body="${3:-}"
  local when
  when="$(date '+%H:%M')"
  if [[ -n "${body}" ]]; then
    body="${body}"$'\n'"${when}"
  else
    body="${when}"
  fi
  export DISPLAY="${DISPLAY:-:0}"
  export WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-0}"
  export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=/run/user/$(id -u)/bus}"
  export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  command -v notify-send >/dev/null 2>&1 || return 0
  # expire-time 0 + not transient: stay in the tray until the user dismisses.
  notify-send --app-name=agent-fleet --urgency="${urgency}" --expire-time=0 \
    --hint=int:transient:0 \
    "${summary}" "${body}" >/dev/null 2>&1 || true
}
