#!/usr/bin/env bash
# Grant users.instance_admin for an existing account.
# Prefer INSTANCE_ADMIN_EMAILS in .env (grant on next login) when you can.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
email="${1:-}"
if [[ -z "$email" ]]; then
  echo "usage: $0 user@example.test" >&2
  exit 1
fi
if [[ -z "${DATABASE_URL:-}" && -f .env ]]; then
  # shellcheck disable=SC1091
  set -a
  # shellcheck source=/dev/null
  source .env
  set +a
fi
if [[ -z "${DATABASE_URL:-}" ]]; then
  echo "DATABASE_URL is required (repo-root .env)." >&2
  exit 1
fi
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c \
  "UPDATE users SET instance_admin = TRUE, updated_at = now()
   WHERE email_normalized = lower(trim('$email'))
   RETURNING email, instance_admin;"
