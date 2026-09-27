#!/bin/sh
# fleet-stack up must restart caddy after compose up. A bind-mounted
# Caddyfile does not change the service hash, so compose leaves the old
# process in place.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
script="${root}/scripts/fleet-stack.sh"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/stack"
log="$tmp/docker.log"

cat > "$tmp/docker" << 'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$FLEET_STACK_LOG"
exit 0
EOF
chmod +x "$tmp/docker"

run_up() {
  FLEET_STACK_DIR="$tmp/stack" \
  FLEET_STACK_DOCKER="$tmp/docker" \
  FLEET_STACK_LOG="$log" \
    sh "$script" up "$1"
}

: > "$log"
run_up d29218d959f4ef05a110975578306df2a5fabf47
grep -F "compose --env-file .env up -d --remove-orphans" "$log" >/dev/null
grep -F "compose --env-file .env restart caddy" "$log" >/dev/null
# restart is after up
awk '
  /compose --env-file .env up -d --remove-orphans/ { up=NR }
  /compose --env-file .env restart caddy/ { restart=NR }
  END { exit !(up && restart && restart > up) }
' "$log"

: > "$log"
if FLEET_STACK_DIR="$tmp/stack" FLEET_STACK_DOCKER="$tmp/docker" FLEET_STACK_LOG="$log" \
  sh "$script" up not-a-sha; then
  echo "invalid sha must fail" >&2
  exit 1
fi
if grep -F "restart caddy" "$log" >/dev/null; then
  echo "invalid sha must not restart caddy" >&2
  exit 1
fi

echo "fleet-stack up restarts caddy"
