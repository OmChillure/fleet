# Sourced by scripts. Edit here if the VPS IP or repo path changes.
# Lives in deploy/studio/. Product repo is two levels up.

ROOT="${ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)}"

VPS_HOST=82.39.213.33
VPS_USER=agent-fleet
VPS_SSH_KEY="${HOME}/.ssh/agent-fleet"
VPS_DEPLOY_USER=agent-fleet-deploy
VPS_DEPLOY_KEY="${HOME}/.ssh/agent-fleet-deploy"
VPS_STACK_DIR=/home/agent-fleet/stack

# Product repo: parent of deploy/studio (this file's directory).
REPO="${REPO:-$(cd "${ROOT}/../.." && pwd)}"
GIT_REMOTE=origin
GIT_BRANCH=main

# Public URL the browser and OAuth callbacks use (Cloudflare → Caddy :80/:443).
DOMAIN=agentfleet.studio
PUBLIC_BASE=https://agentfleet.studio

HEALTH_TIMEOUT_SECS=240
STATE_DIR="${HOME}/.local/state/vps-fleet"
