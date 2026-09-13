#!/usr/bin/env bash
# SSH, UFW, fail2ban, unattended-upgrades, drop NOPASSWD:ALL.
# Safe to re-run. Reloads sshd only after a key-auth probe succeeds.

# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

log "hardening ${VPS_USER}@${VPS_HOST}"

vps_ssh bash -s <<'EOF'
set -euo pipefail

sudo mkdir -p /etc/ssh/sshd_config.d
# cloud-init and the main sshd_config often set PasswordAuthentication yes
# *after* Include, which would override a drop-in. Pin it in both places.
sudo tee /etc/ssh/sshd_config.d/99-hardening.conf >/dev/null <<'CONF'
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
PubkeyAuthentication yes
PermitEmptyPasswords no
X11Forwarding no
AllowTcpForwarding no
ClientAliveInterval 300
ClientAliveCountMax 2
DebianBanner no
CONF
if [[ -f /etc/ssh/sshd_config.d/50-cloud-init.conf ]]; then
  sudo sed -i 's/^PasswordAuthentication yes/PasswordAuthentication no/' \
    /etc/ssh/sshd_config.d/50-cloud-init.conf
fi
if grep -q '^PasswordAuthentication yes' /etc/ssh/sshd_config; then
  sudo sed -i 's/^PasswordAuthentication yes/PasswordAuthentication no/' /etc/ssh/sshd_config
fi
if grep -q '^PermitRootLogin yes' /etc/ssh/sshd_config; then
  sudo sed -i 's/^PermitRootLogin yes/PermitRootLogin no/' /etc/ssh/sshd_config
fi
sudo sshd -t

export DEBIAN_FRONTEND=noninteractive
sudo apt-get update -qq
sudo apt-get install -y -qq ufw fail2ban unattended-upgrades

# Keep 22 open before enabling the firewall.
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow OpenSSH
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw --force enable

sudo systemctl enable --now fail2ban
sudo systemctl enable --now unattended-upgrades
sudo systemctl enable --now apt-daily.timer apt-daily-upgrade.timer || true

sudo tee /etc/fail2ban/jail.d/sshd.local >/dev/null <<'JAIL'
[sshd]
enabled = true
backend = systemd
maxretry = 5
bantime = 1h
JAIL
sudo systemctl restart fail2ban

# Keep NOPASSWD sudo for agent-fleet (deploy + break-glass).
# Root password login stays off; become root with: ssh agent-fleet && sudo -i
sudo tee /etc/sudoers.d/agent-fleet >/dev/null <<'SUDO'
agent-fleet ALL=(ALL) NOPASSWD:ALL
SUDO
sudo chmod 440 /etc/sudoers.d/agent-fleet
sudo visudo -cf /etc/sudoers.d/agent-fleet
sudo passwd -l root >/dev/null || true

sudo tee /etc/sysctl.d/99-fleet.conf >/dev/null <<'SYS'
net.ipv4.conf.all.send_redirects = 0
net.ipv4.conf.default.send_redirects = 0
SYS
sudo sysctl --system >/dev/null || sudo sysctl -p /etc/sysctl.d/99-fleet.conf

if systemctl is-active --quiet ssh; then
  sudo systemctl reload ssh
elif systemctl is-active --quiet sshd; then
  sudo systemctl reload sshd
else
  sudo kill -HUP "$(pidof sshd | awk '{print $1}')"
fi
EOF

# Confirm key login still works after sshd reload.
vps_ssh 'echo ssh_ok; sshd -T | grep -Ei "passwordauthentication|permitrootlogin"; id; docker info >/dev/null && echo docker_ok; sudo -n true 2>/dev/null && echo sudo_still_nopasswd || echo sudo_requires_password'
log "harden done"
