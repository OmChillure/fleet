#!/usr/bin/env bash
# Start a local Gitea and seed users, repos, and one issue for Fleet tests.
# Prints an access token and an OAuth app for GITEA_CLIENT_ID / GITEA_CLIENT_SECRET.
#
#   scripts/gitea-up.sh
#   GITEA_BASE_URL=http://localhost:3033
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BASE="${GITEA_BASE_URL:-http://localhost:3033}"
BASE="${BASE%/}"
ADMIN_USER="${GITEA_ADMIN_USER:-fleet}"
ADMIN_PASS="${GITEA_ADMIN_PASS:-FleetDevPass123}"
ADMIN_EMAIL="${GITEA_ADMIN_EMAIL:-fleet@example.com}"
DEV_USER="${GITEA_DEV_USER:-dev}"
DEV_PASS="${GITEA_DEV_PASS:-DevPass123456}"
DEV_EMAIL="${GITEA_DEV_EMAIL:-dev@example.com}"
REDIRECT="${GITEA_OAUTH_REDIRECT:-http://localhost:8080/api/integrations/oauth/callback/gitea}"

docker compose up -d gitea
echo "waiting for $BASE"
ready=0
for _ in $(seq 1 60); do
  if curl -fsS "$BASE/api/v1/version" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 2
done
if [[ "$ready" -ne 1 ]]; then
  echo "gitea did not become ready at $BASE" >&2
  exit 1
fi

create_user() {
  local user="$1" pass="$2" email="$3" admin_flag="$4"
  if docker compose exec -u git -T gitea gitea admin user create \
    --username "$user" --password "$pass" --email "$email" $admin_flag \
    --must-change-password=false >/dev/null 2>&1; then
    echo "created user $user"
  else
    echo "user $user already exists"
  fi
}
create_user "$ADMIN_USER" "$ADMIN_PASS" "$ADMIN_EMAIL" "--admin"
create_user "$DEV_USER" "$DEV_PASS" "$DEV_EMAIL" ""

TOKEN="$(docker compose exec -u git -T gitea gitea admin user generate-access-token \
  --username "$ADMIN_USER" --token-name "fleet-seed-$(date +%s)" --raw --scopes all | tr -d '\r')"
if [[ -z "$TOKEN" ]]; then
  echo "could not create an access token" >&2
  exit 1
fi

api() {
  local method="$1" path="$2" body="${3:-}"
  if [[ -n "$body" ]]; then
    curl -fsS -X "$method" "$BASE/api/v1$path" \
      -H "Authorization: token $TOKEN" \
      -H "Content-Type: application/json" \
      -d "$body"
  else
    curl -fsS -X "$method" "$BASE/api/v1$path" \
      -H "Authorization: token $TOKEN"
  fi
}

api POST /orgs "{\"username\":\"worms\",\"visibility\":\"public\",\"description\":\"Fleet test org\"}" >/dev/null || echo "org worms already exists"
api POST /orgs/worms/repos "{\"name\":\"game\",\"auto_init\":true,\"default_branch\":\"main\",\"private\":false,\"description\":\"Three.js worms game\"}" >/dev/null || echo "repo worms/game already exists"
api POST /orgs/worms/repos "{\"name\":\"docs\",\"auto_init\":true,\"default_branch\":\"main\",\"private\":false,\"description\":\"Notes\"}" >/dev/null || echo "repo worms/docs already exists"
api POST /repos/worms/game/issues "{\"title\":\"Add a worm\",\"body\":\"A Three.js worm that moves on a grid. No git remote is required inside Fleet; this issue is the task.\"}" >/dev/null || true
api PUT /orgs/worms/members/"$DEV_USER" >/dev/null || echo "could not add $DEV_USER to worms (may already be a member)"

OAUTH="$(api POST /user/applications/oauth2 "{\"name\":\"agent-fleet\",\"redirect_uris\":[\"$REDIRECT\"],\"confidential_client\":true}")"
CLIENT_ID="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("client_id",""))' <<<"$OAUTH")"
CLIENT_SECRET="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("client_secret",""))' <<<"$OAUTH")"

mkdir -p "$ROOT/.tmp"
umask 077
cat > "$ROOT/.tmp/gitea.env" <<EOF
GITEA_BASE_URL=$BASE
GITEA_ADMIN_USER=$ADMIN_USER
GITEA_ADMIN_PASS=$ADMIN_PASS
GITEA_DEV_USER=$DEV_USER
GITEA_DEV_PASS=$DEV_PASS
GITEA_TOKEN=$TOKEN
GITEA_CLIENT_ID=$CLIENT_ID
GITEA_CLIENT_SECRET=$CLIENT_SECRET
EOF

echo "Gitea is up at $BASE"
echo "  admin $ADMIN_USER / $ADMIN_PASS"
echo "  user  $DEV_USER / $DEV_PASS"
echo "  repos worms/game (issue: Add a worm), worms/docs"
echo "  token and OAuth client written to .tmp/gitea.env"
echo "  Connect Fleet with GITEA_BASE_URL=$BASE GITEA_CLIENT_ID GITEA_CLIENT_SECRET from that file"
