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
