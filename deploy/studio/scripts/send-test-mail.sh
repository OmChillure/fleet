#!/usr/bin/env bash
# Send one test message via Resend (HTTP API) using the VPS .env.
# Usage: ./scripts/send-test-mail.sh you@example.com

# shellcheck source=lib.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/lib.sh"

TO="${1:?usage: $0 you@example.com}"

log "sending test mail to ${TO} from ${VPS_HOST}"

vps_ssh "TO='${TO}' STACK_DIR='${VPS_STACK_DIR}' bash -s" <<'EOF'
set -euo pipefail
set -a
# shellcheck disable=SC1091
source "${STACK_DIR}/.env"
set +a

if [[ -z "${RESEND_API_KEY:-}" && -z "${SMTP_PASSWORD:-}" ]]; then
  echo "RESEND_API_KEY / SMTP_PASSWORD empty in ${STACK_DIR}/.env — see mail.md" >&2
  exit 1
fi

FROM="${SMTP_FROM:-Agent Fleet <noreply@mail.agentfleet.studio>}"
REPLY="${SMTP_REPLY_TO:-hello@agentfleet.studio}"
KEY="${RESEND_API_KEY:-${SMTP_PASSWORD}}"

export FROM REPLY
payload="$(python3 -c 'import json,os
print(json.dumps({
  "from": os.environ["FROM"],
  "to": [os.environ["TO"]],
  "reply_to": os.environ["REPLY"],
  "subject": "agentfleet.studio test",
  "text": "If you can read this, outbound Resend works.",
}))')"

code="$(curl -sS -o /tmp/resend-out -w '%{http_code}' \
  -X POST https://api.resend.com/emails \
  -H "Authorization: Bearer ${KEY}" \
  -H "Content-Type: application/json" \
  -d "${payload}")"

if [[ "${code}" != 200 ]]; then
  echo "Resend HTTP ${code}:" >&2
  cat /tmp/resend-out >&2
  echo >&2
  exit 1
fi
echo "sent $(cat /tmp/resend-out)"
EOF
