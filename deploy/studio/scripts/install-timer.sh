#!/usr/bin/env bash
# Install a user systemd timer that spawns a Grok deploy session every 5 minutes.

# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

UNIT_DIR="${HOME}/.config/systemd/user"
mkdir -p "${UNIT_DIR}"

sed "s|@ROOT@|${ROOT}|g" "${ROOT}/systemd/agent-fleet-deploy.service" \
  > "${UNIT_DIR}/agent-fleet-deploy.service"
cp "${ROOT}/systemd/agent-fleet-deploy.timer" "${UNIT_DIR}/agent-fleet-deploy.timer"

systemctl --user daemon-reload
systemctl --user enable agent-fleet-deploy.timer
# Restart so a copied unit replaces an already-active (possibly elapsed) timer.
systemctl --user restart agent-fleet-deploy.timer

if command -v loginctl >/dev/null; then
  loginctl enable-linger "${USER}" >/dev/null || true
fi

log "timer installed. status:"
systemctl --user status --no-pager agent-fleet-deploy.timer || true
log "spawn a session now: systemctl --user start agent-fleet-deploy.service"
log "logs: journalctl --user -u agent-fleet-deploy.service -f"
log "session id: cat ~/.local/state/vps-fleet/grok-session"
