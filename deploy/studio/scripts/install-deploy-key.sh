#!/usr/bin/env bash
# Create agent-fleet-deploy: no sudo, no docker group. May only run
# /usr/local/sbin/fleet-stack via NOPASSWD. Admin key stays on agent-fleet.

# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

if [[ ! -f "${VPS_DEPLOY_KEY}" ]]; then
  ssh-keygen -t ed25519 -C "agent-fleet-deploy" -f "${VPS_DEPLOY_KEY}" -N "" -q
  chmod 600 "${VPS_DEPLOY_KEY}"
  chmod 644 "${VPS_DEPLOY_KEY}.pub"
  log "created ${VPS_DEPLOY_KEY}"
fi

PUB="$(cat "${VPS_DEPLOY_KEY}.pub")"

vps_scp "${ROOT}/scripts/fleet-stack.sh" "${VPS_USER}@${VPS_HOST}:/tmp/fleet-stack.sh"
vps_scp "${ROOT}/scripts/fleet-backup.sh" "${VPS_USER}@${VPS_HOST}:/tmp/fleet-backup.sh"
printf '%s\n' "${PUB}" > /tmp/agent-fleet-deploy.pub
vps_scp /tmp/agent-fleet-deploy.pub "${VPS_USER}@${VPS_HOST}:/tmp/agent-fleet-deploy.pub"
rm -f /tmp/agent-fleet-deploy.pub

vps_ssh bash -s <<'EOF'
set -euo pipefail
id agent-fleet-deploy &>/dev/null || sudo adduser --disabled-password --gecos "" agent-fleet-deploy
# Must not be in sudo or docker.
sudo gpasswd -d agent-fleet-deploy sudo 2>/dev/null || true
sudo gpasswd -d agent-fleet-deploy docker 2>/dev/null || true
sudo usermod -aG agent-fleet agent-fleet-deploy

sudo install -o root -g root -m 755 /tmp/fleet-stack.sh /usr/local/sbin/fleet-stack
sudo install -o root -g root -m 755 /tmp/fleet-backup.sh /usr/local/sbin/fleet-backup
rm -f /tmp/fleet-stack.sh /tmp/fleet-backup.sh

sudo tee /etc/sudoers.d/agent-fleet-deploy >/dev/null <<'SUDO'
agent-fleet-deploy ALL=(root) NOPASSWD: /usr/local/sbin/fleet-stack
SUDO
sudo chmod 440 /etc/sudoers.d/agent-fleet-deploy
sudo visudo -cf /etc/sudoers.d/agent-fleet-deploy

# Stack writable by deploy group; .env and certs stay owner-only.
sudo chown -R agent-fleet:agent-fleet /home/agent-fleet/stack
sudo chmod 750 /home/agent-fleet
sudo chmod 2770 /home/agent-fleet/stack
sudo chmod 664 /home/agent-fleet/stack/compose.yml /home/agent-fleet/stack/Caddyfile 2>/dev/null || true
sudo chmod 600 /home/agent-fleet/stack/.env
sudo chmod 700 /home/agent-fleet/stack/certs 2>/dev/null || true
sudo chmod 600 /home/agent-fleet/stack/certs/origin.key 2>/dev/null || true
sudo touch /home/agent-fleet/stack/DEPLOYED_SHA /home/agent-fleet/stack/PREVIOUS_SHA
sudo chown agent-fleet:agent-fleet /home/agent-fleet/stack/DEPLOYED_SHA /home/agent-fleet/stack/PREVIOUS_SHA
sudo chmod 664 /home/agent-fleet/stack/DEPLOYED_SHA /home/agent-fleet/stack/PREVIOUS_SHA

sudo install -d -m 700 -o agent-fleet-deploy -g agent-fleet-deploy /home/agent-fleet-deploy/.ssh
sudo cp /tmp/agent-fleet-deploy.pub /home/agent-fleet-deploy/.ssh/authorized_keys
sudo rm -f /tmp/agent-fleet-deploy.pub
sudo chown agent-fleet-deploy:agent-fleet-deploy /home/agent-fleet-deploy/.ssh/authorized_keys
sudo chmod 600 /home/agent-fleet-deploy/.ssh/authorized_keys

sudo tee /etc/cron.d/fleet-backup >/dev/null <<'CRON'
15 3 * * * root /usr/local/sbin/fleet-backup >/var/log/fleet-backup.log 2>&1
CRON
sudo chmod 644 /etc/cron.d/fleet-backup
EOF

# Prove the deploy key works and cannot sudo arbitrarily.
ssh -i "${VPS_DEPLOY_KEY}" -o BatchMode=yes -o IdentitiesOnly=yes \
  "${VPS_DEPLOY_USER}@${VPS_HOST}" \
  'echo deploy_ssh_ok; groups; sudo -n true 2>&1 | head -1; sudo -n /usr/local/sbin/fleet-stack 2>&1 | head -1; id'
log "deploy key installed"
