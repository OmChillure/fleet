#!/bin/bash
# Postgres dump. Installed as /usr/local/sbin/fleet-backup (root cron).
set -euo pipefail

STACK=/home/agent-fleet/stack
DEST=/home/agent-fleet/backups
SHA="$(cat "${STACK}/DEPLOYED_SHA" 2>/dev/null || true)"
[[ -n "${SHA}" ]] || { echo "no DEPLOYED_SHA" >&2; exit 1; }

mkdir -p "${DEST}"
chmod 700 "${DEST}"
chown agent-fleet:agent-fleet "${DEST}"

file="${DEST}/fleet-$(date -u +%Y%m%dT%H%M%SZ).sql.gz"
env FLEET_GIT_SHA="${SHA}" /usr/bin/docker compose \
  --project-directory "${STACK}" --env-file "${STACK}/.env" \
  exec -T postgres pg_dump -U fleet fleet | gzip -c > "${file}"
chown agent-fleet:agent-fleet "${file}"
chmod 600 "${file}"

find "${DEST}" -type f -name 'fleet-*.sql.gz' -mtime +7 -delete
echo "wrote ${file}"
