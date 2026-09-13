#!/usr/bin/env bash
# One-time VPS setup. Does not talk to GitLab. Does not overwrite an existing .env.

# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

log "bootstrapping ${VPS_USER}@${VPS_HOST}:${VPS_STACK_DIR}"

vps_ssh "sudo usermod -aG docker '${VPS_USER}'"

vps_ssh bash -s <<EOF
set -euo pipefail
sudo mkdir -p '${VPS_STACK_DIR}'
sudo chown '${VPS_USER}:${VPS_USER}' '${VPS_STACK_DIR}'
chmod 700 '${VPS_STACK_DIR}'
EOF

vps_scp "${ROOT}/compose.yml" "${VPS_USER}@${VPS_HOST}:${VPS_STACK_DIR}/compose.yml"
vps_scp "${ROOT}/Caddyfile" "${VPS_USER}@${VPS_HOST}:${VPS_STACK_DIR}/Caddyfile"

vps_ssh "STACK_DIR='${VPS_STACK_DIR}' DOMAIN='${DOMAIN}' bash -s" <<'EOF'
set -euo pipefail
mkdir -p "${STACK_DIR}/certs"
if [[ ! -f "${STACK_DIR}/certs/origin.crt" ]]; then
  openssl req -x509 -newkey rsa:2048 -sha256 -days 825 -nodes \
    -keyout "${STACK_DIR}/certs/origin.key" \
    -out "${STACK_DIR}/certs/origin.crt" \
    -subj "/CN=${DOMAIN}" \
    -addext "subjectAltName=DNS:${DOMAIN},DNS:www.${DOMAIN}"
  chmod 600 "${STACK_DIR}/certs/origin.key"
  chmod 644 "${STACK_DIR}/certs/origin.crt"
fi
EOF

vps_ssh "STACK_DIR='${VPS_STACK_DIR}' PUBLIC_BASE='${PUBLIC_BASE}' bash -s" <<'EOF'
set -euo pipefail
cd "${STACK_DIR}"
if [[ -f .env ]]; then
  echo "keeping existing ${STACK_DIR}/.env"
else
  umask 077
  {
    printf 'POSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 24)"
    printf 'JWT_SECRET=%s\n' "$(openssl rand -hex 32)"
    printf 'FLEET_BOOTSTRAP_SECRET=%s\n' "$(openssl rand -hex 24)"
    printf 'INTEGRATIONS_ENCRYPTION_KEY=%s\n' "$(openssl rand -hex 32)"
    printf 'API_PUBLIC_BASE=%s\n' "${PUBLIC_BASE}"
    printf 'UI_PUBLIC_BASE=%s\n' "${PUBLIC_BASE}"
    printf 'CORS_ORIGINS=%s,https://www.agentfleet.studio\n' "${PUBLIC_BASE}"
    printf 'COOKIE_SECURE=true\n'
    printf 'COOKIE_SAMESITE=Lax\n'
    printf 'FLEET_HARNESS=mock\n'
    printf 'SIGNUP_TOKEN=%s\n' "$(openssl rand -hex 16)"
    printf 'SMTP_HOST=smtp.resend.com\n'
    printf 'SMTP_PORT=465\n'
    printf 'SMTP_USERNAME=resend\n'
    printf 'SMTP_PASSWORD=\n'
    printf 'SMTP_FROM=Agent Fleet <noreply@mail.agentfleet.studio>\n'
    printf 'SMTP_REPLY_TO=hello@agentfleet.studio\n'
    printf 'RESEND_API_KEY=\n'
  } > .env
  chmod 600 .env
  echo "wrote ${STACK_DIR}/.env (secrets stay on the VPS)"
fi

if command -v ufw >/dev/null && sudo ufw status 2>/dev/null | grep -q 'Status: active'; then
  sudo ufw allow 80/tcp comment 'agent-fleet http' || true
  sudo ufw allow 443/tcp comment 'agent-fleet https' || true
fi
EOF

log "bootstrap done. next: ${ROOT}/scripts/deploy.sh"
