#!/usr/bin/env bash
# Start a local Gogs and seed users, repos, and one issue for Fleet tests.
#
#   scripts/gogs-up.sh
#   GOGS_BASE_URL=http://localhost:3034
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
BASE="${GOGS_BASE_URL:-http://localhost:3034}"
BASE="${BASE%/}"
ADMIN_USER="${GOGS_ADMIN_USER:-fleet}"
ADMIN_PASS="${GOGS_ADMIN_PASS:-FleetDevPass123}"
ADMIN_EMAIL="${GOGS_ADMIN_EMAIL:-fleet@example.com}"
DEV_USER="${GOGS_DEV_USER:-dev}"
DEV_PASS="${GOGS_DEV_PASS:-DevPass123456}"
DEV_EMAIL="${GOGS_DEV_EMAIL:-dev@example.com}"

docker compose --profile gogs up -d gogs
echo "waiting for $BASE"
ready=0
for _ in $(seq 1 60); do
  code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/user/login" || true)"
  if [[ "$code" == "200" ]]; then
    ready=1
    break
  fi
  # First boot serves the install form.
  install_code="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/install" || true)"
  if [[ "$install_code" == "200" ]]; then
    ready=1
    break
  fi
  sleep 2
done
if [[ "$ready" -ne 1 ]]; then
  echo "gogs did not become ready at $BASE" >&2
  exit 1
fi

if curl -fsS "$BASE/install" >/dev/null 2>&1; then
  curl -fsS -X POST "$BASE/install" \
    -H "Content-Type: application/x-www-form-urlencoded" \
    --data-urlencode "db_type=SQLite3" \
    --data-urlencode "db_path=/data/gogs/data/gogs.db" \
    --data-urlencode "app_name=Gogs" \
    --data-urlencode "repo_root_path=/data/git/gogs-repositories" \
    --data-urlencode "run_user=git" \
    --data-urlencode "domain=localhost" \
    --data-urlencode "ssh_port=22" \
    --data-urlencode "http_port=3000" \
    --data-urlencode "app_url=${BASE}/" \
    --data-urlencode "log_root_path=/data/gogs/log" \
    --data-urlencode "disable_registration=on" \
    --data-urlencode "admin_name=${ADMIN_USER}" \
    --data-urlencode "admin_passwd=${ADMIN_PASS}" \
    --data-urlencode "admin_confirm_passwd=${ADMIN_PASS}" \
    --data-urlencode "admin_email=${ADMIN_EMAIL}" \
    >/dev/null
  echo "installed gogs"
fi

api() {
  local method="$1" path="$2" body="${3:-}" auth="${4:-token}"
  local -a args=(-fsS -X "$method" "$BASE/api/v1$path" -H "Accept: application/json")
  if [[ "$auth" == "basic" ]]; then
    args+=(-u "$ADMIN_USER:$ADMIN_PASS")
  else
    args+=(-H "Authorization: token $TOKEN")
  fi
  if [[ -n "$body" ]]; then
    args+=(-H "Content-Type: application/json" -d "$body")
  fi
  curl "${args[@]}"
}

TOKEN="$(api POST "/users/${ADMIN_USER}/tokens" "{\"name\":\"fleet-seed-$(date +%s)\"}" basic | python3 -c 'import json,sys; print(json.load(sys.stdin).get("sha1",""))')"
if [[ -z "$TOKEN" ]]; then
  echo "could not create an access token" >&2
  exit 1
fi

api POST /admin/users "{\"username\":\"${DEV_USER}\",\"email\":\"${DEV_EMAIL}\",\"password\":\"${DEV_PASS}\",\"login_name\":\"${DEV_USER}\"}" >/dev/null || echo "user $DEV_USER already exists"
api POST /admin/users/"$ADMIN_USER"/orgs "{\"username\":\"worms\",\"full_name\":\"Fleet test org\"}" >/dev/null || echo "org worms already exists"
# Gogs 0.14 auto_init fails (readme template is a directory). Create empty, then commit.
api POST /org/worms/repos "{\"name\":\"game\",\"private\":false,\"description\":\"Three.js worms game\"}" >/dev/null || echo "repo worms/game already exists"
api POST /org/worms/repos "{\"name\":\"docs\",\"private\":false,\"description\":\"Notes\"}" >/dev/null || echo "repo worms/docs already exists"
seed_commit() {
  local repo="$1"
  docker compose exec -u git -T gogs sh -c "
    set -e
    bare=/data/git/gogs-repositories/worms/${repo}.git
    if git --git-dir=\"\$bare\" rev-parse --verify refs/heads/master >/dev/null 2>&1; then
      exit 0
    fi
    work=\$(mktemp -d)
    git -C \"\$work\" init -q -b master
    echo '# ${repo}' > \"\$work/README.md\"
    git -C \"\$work\" add README.md
    git -C \"\$work\" -c user.email=fleet@example.com -c user.name=fleet commit -q -m 'init'
    git -C \"\$work\" remote add origin \"\$bare\"
    git -C \"\$work\" push -q origin master
    git --git-dir=\"\$bare\" symbolic-ref HEAD refs/heads/master
    rm -rf \"\$work\"
  "
}
seed_commit game
seed_commit docs
api POST /repos/worms/game/issues "{\"title\":\"Add a worm\",\"body\":\"A Three.js worm that moves on a grid. No git remote is required inside Fleet; this issue is the task.\"}" >/dev/null || true

mkdir -p "$ROOT/.tmp"
umask 077
cat > "$ROOT/.tmp/gogs.env" <<EOF
GOGS_BASE_URL=$BASE
GOGS_ADMIN_USER=$ADMIN_USER
GOGS_ADMIN_PASS=$ADMIN_PASS
GOGS_DEV_USER=$DEV_USER
GOGS_DEV_PASS=$DEV_PASS
GOGS_TOKEN=$TOKEN
EOF

echo "Gogs is up at $BASE"
echo "  admin $ADMIN_USER / $ADMIN_PASS"
echo "  user  $DEV_USER / $DEV_PASS"
echo "  repos worms/game (issue: Add a worm), worms/docs"
echo "  token written to .tmp/gogs.env"
echo "  Connect Gogs in Fleet with that username and password. GOGS_BASE_URL=$BASE"
