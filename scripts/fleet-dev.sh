#!/usr/bin/env bash
# fleet-dev.sh — set up and run Agent Fleet locally (postgres + API + UI).
#
# Processes run in the background via nohup; PIDs/logs live under .run/.
#
# Usage:
#   ./scripts/fleet-dev.sh setup      # .env, docker postgres, cargo build, tool checks
#   ./scripts/fleet-dev.sh start      # setup deps if needed, then start (nohup)
#   ./scripts/fleet-dev.sh stop       # stop API + UI (postgres left running)
#   ./scripts/fleet-dev.sh restart    # stop + start
#   ./scripts/fleet-dev.sh status     # PIDs, ports, health
#   ./scripts/fleet-dev.sh logs [all|server|ui]   # tail logs (default: all)
#   ./scripts/fleet-dev.sh down       # stop processes + docker compose down
#
# Env overrides:
#   FLEET_SKIP_BUILD=1   skip cargo build on start
#   FLEET_SKIP_UI=1      do not start the UI
#   FLEET_UI_PORT=3030
#   FLEET_API_PORT=8080  (must match .env PORT if set)
#   COMPOSE_FILE=...     docker compose file (default: docker-compose.yml)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

RUN_DIR="${FLEET_RUN_DIR:-$ROOT/.run}"
LOG_DIR="$RUN_DIR/logs"
PID_SERVER="$RUN_DIR/fleet-server.pid"
PID_UI="$RUN_DIR/fleet-ui.pid"
LOG_SERVER="$LOG_DIR/fleet-server.log"
LOG_UI="$LOG_DIR/fleet-ui.log"

COMPOSE_FILE="${COMPOSE_FILE:-$ROOT/docker-compose.yml}"
UI_PORT="${FLEET_UI_PORT:-3030}"
API_PORT="${FLEET_API_PORT:-8080}"

# Matches docker-compose.yml (postgres:16 on host 5433).
COMPOSE_DATABASE_URL="postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable"
EXAMPLE_DB_URL="postgres://postgres:postgres@127.0.0.1:5432/agent_fleet"

RED=$'\033[0;31m'
GRN=$'\033[0;32m'
YLW=$'\033[0;33m'
BLU=$'\033[0;34m'
RST=$'\033[0m'

log()  { printf '%s%s%s\n' "$BLU" "$*" "$RST"; }
ok()   { printf '%s%s%s\n' "$GRN" "$*" "$RST"; }
warn() { printf '%s%s%s\n' "$YLW" "$*" "$RST"; }
err()  { printf '%s%s%s\n' "$RED" "$*" "$RST" >&2; }

usage() {
  sed -n '2,20p' "$0" | sed 's/^# \?//'
  exit "${1:-0}"
}

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    err "missing required command: $1"
    return 1
  fi
}

mkdir_run() {
  mkdir -p "$RUN_DIR" "$LOG_DIR" "$ROOT/data"
}

compose() {
  if docker compose version >/dev/null 2>&1; then
    docker compose -f "$COMPOSE_FILE" "$@"
  else
    docker-compose -f "$COMPOSE_FILE" "$@"
  fi
}

# Load KEY=VAL from .env without executing it (simple export).
load_dotenv() {
  local f="$ROOT/.env"
  [[ -f "$f" ]] || return 0
  set -a
  # shellcheck disable=SC1090
  source <(grep -E '^[A-Za-z_][A-Za-z0-9_]*=' "$f" | sed 's/\r$//')
  set +a
}

is_pid_alive() {
  local pid="${1:-}"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

read_pid() {
  local file="$1"
  if [[ -f "$file" ]]; then
    tr -d '[:space:]' <"$file" || true
  fi
}

port_pids() {
  local port="$1"
  if command -v lsof >/dev/null 2>&1; then
    lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true
  elif command -v fuser >/dev/null 2>&1; then
    fuser "${port}/tcp" 2>/dev/null | tr -s ' ' '\n' | grep -E '^[0-9]+$' || true
  else
    return 0
  fi
}

wait_http() {
  local url="$1"
  local name="$2"
  local attempts="${3:-60}"
  local i
  for i in $(seq 1 "$attempts"); do
    if curl -sf -o /dev/null --max-time 2 "$url" 2>/dev/null; then
      ok "  $name is up ($url)"
      return 0
    fi
    sleep 1
  done
  warn "  $name did not become ready within ${attempts}s — check $LOG_DIR"
  return 1
}

wait_postgres() {
  local attempts="${1:-40}"
  local i
  log "Waiting for Postgres (docker compose health)…"
  for i in $(seq 1 "$attempts"); do
    if compose exec -T postgres pg_isready -U fleet -d fleet >/dev/null 2>&1; then
      ok "  Postgres ready on localhost:5433"
      return 0
    fi
    sleep 1
  done
  err "Postgres not ready — try: docker compose -f $COMPOSE_FILE logs postgres"
  return 1
}

ensure_env() {
  mkdir_run
  if [[ ! -f "$ROOT/.env" ]]; then
    if [[ ! -f "$ROOT/.env.example" ]]; then
      err "no .env and no .env.example"
      exit 1
    fi
    cp "$ROOT/.env.example" "$ROOT/.env"
    ok "Created .env from .env.example"
  fi

  # Point DATABASE_URL at compose Postgres when still on the generic example value
  # or when missing.
  if ! grep -qE '^DATABASE_URL=' "$ROOT/.env"; then
    echo "DATABASE_URL=$COMPOSE_DATABASE_URL" >>"$ROOT/.env"
    warn "Appended DATABASE_URL for docker compose Postgres"
  else
    local current
    current="$(grep -E '^DATABASE_URL=' "$ROOT/.env" | head -1 | cut -d= -f2-)"
    if [[ "$current" == "$EXAMPLE_DB_URL" ]] || [[ "$current" == *"127.0.0.1:5432/agent_fleet"* ]]; then
      if grep -qE '^DATABASE_URL=' "$ROOT/.env"; then
        # portable in-place rewrite
        local tmp
        tmp="$(mktemp)"
        sed "s|^DATABASE_URL=.*|DATABASE_URL=$COMPOSE_DATABASE_URL|" "$ROOT/.env" >"$tmp"
        mv "$tmp" "$ROOT/.env"
        warn "Set DATABASE_URL to compose Postgres ($COMPOSE_DATABASE_URL)"
      fi
    fi
  fi

  if ! grep -qE '^DATA_DIR=' "$ROOT/.env"; then
    echo "DATA_DIR=./data" >>"$ROOT/.env"
  fi

  # Ensure JWT_SECRET length note is satisfied (example already is).
  if ! grep -qE '^JWT_SECRET=.{32,}' "$ROOT/.env"; then
    warn "JWT_SECRET should be ≥ 32 characters (see .env)"
  fi

  # Optional ADMIN_TOKEN for demo-orch.sh — leave commented unless present.
  load_dotenv
  export PORT="${PORT:-$API_PORT}"
  export HOST="${HOST:-127.0.0.1}"
}

start_postgres() {
  need_cmd docker
  log "Starting Postgres (docker compose)…"
  compose up -d postgres
  wait_postgres
}

stop_pid_file() {
  local name="$1"
  local pidfile="$2"
  local pid
  pid="$(read_pid "$pidfile")"
  if is_pid_alive "$pid"; then
    log "Stopping $name (pid $pid)…"
    kill "$pid" 2>/dev/null || true
    # Give a moment, then force if needed.
    for _ in 1 2 3 4 5; do
      is_pid_alive "$pid" || break
      sleep 0.4
    done
    if is_pid_alive "$pid"; then
      warn "  $name still alive — SIGKILL"
      kill -9 "$pid" 2>/dev/null || true
    fi
    ok "  stopped $name"
  elif [[ -n "$pid" ]]; then
    warn "  $name pid $pid not running (stale pidfile)"
  fi
  rm -f "$pidfile"
}

# Kill anything still listening on our ports (covers orphaned children of cargo/dx).
free_port() {
  local port="$1"
  local name="$2"
  local pids
  pids="$(port_pids "$port")"
  if [[ -z "$pids" ]]; then
    return 0
  fi
  warn "Freeing port $port ($name): $pids"
  # shellcheck disable=SC2086
  kill $pids 2>/dev/null || true
  sleep 0.5
  pids="$(port_pids "$port")"
  if [[ -n "$pids" ]]; then
    # shellcheck disable=SC2086
    kill -9 $pids 2>/dev/null || true
  fi
}

cmd_setup() {
  log "=== Fleet setup ==="
  ensure_env
  need_cmd cargo
  need_cmd curl
  need_cmd docker

  if ! command -v dx >/dev/null 2>&1; then
    warn "dx (Dioxus CLI) not found — UI will not start."
    warn "  Install a 0.7.x CLI matching the workspace dioxus pin, e.g.:"
    warn "    cargo install dioxus-cli --version 0.7.3 --locked"
  fi

  start_postgres

  if [[ "${FLEET_SKIP_BUILD:-0}" != "1" ]]; then
    log "Building fleet-server (first time can take a while)…"
    cargo build -p fleet-server
    ok "  fleet-server built"
  else
    warn "Skipping cargo build (FLEET_SKIP_BUILD=1)"
  fi

  ok "Setup complete."
  echo "  .env          → $ROOT/.env"
  echo "  Postgres      → localhost:5433 (user/db: fleet)"
  echo "  DATA_DIR      → $ROOT/data"
  echo "  Run dir/logs  → $RUN_DIR"
  echo
  echo "Next: ./scripts/fleet-dev.sh start"
}

start_server() {
  load_dotenv
  local port="${PORT:-$API_PORT}"
  if [[ -n "$(port_pids "$port")" ]]; then
    warn "Port $port already in use — not starting fleet-server"
    return 0
  fi
  local pid
  pid="$(read_pid "$PID_SERVER")"
  if is_pid_alive "$pid"; then
    warn "fleet-server already running (pid $pid)"
    return 0
  fi

  log "Starting fleet-server on :$port (nohup)…"
  # shellcheck disable=SC2086
  nohup env \
    CARGO_TERM_COLOR=never \
    cargo run -p fleet-server \
    >>"$LOG_SERVER" 2>&1 &
  echo $! >"$PID_SERVER"
  ok "  fleet-server pid $(read_pid "$PID_SERVER")  log → $LOG_SERVER"
}

start_ui() {
  if [[ "${FLEET_SKIP_UI:-0}" == "1" ]]; then
    warn "Skipping UI (FLEET_SKIP_UI=1)"
    return 0
  fi
  if ! command -v dx >/dev/null 2>&1; then
    warn "dx not found — skip UI. Install: cargo install dioxus-cli --version 0.7.3 --locked"
    return 0
  fi
  if [[ -n "$(port_pids "$UI_PORT")" ]]; then
    warn "Port $UI_PORT already in use — not starting fleet-ui"
    return 0
  fi
  local pid
  pid="$(read_pid "$PID_UI")"
  if is_pid_alive "$pid"; then
    warn "fleet-ui already running (pid $pid)"
    return 0
  fi

  log "Starting fleet-ui on :$UI_PORT (nohup)…"
  nohup env \
    CARGO_TERM_COLOR=never \
    dx serve --package fleet-ui --port "$UI_PORT" \
    >>"$LOG_UI" 2>&1 &
  echo $! >"$PID_UI"
  ok "  fleet-ui pid $(read_pid "$PID_UI")  log → $LOG_UI"
}

cmd_start() {
  log "=== Fleet start ==="
  ensure_env
  need_cmd cargo
  need_cmd curl

  # Postgres first
  if ! compose ps --status running 2>/dev/null | grep -q postgres; then
    start_postgres
  else
    wait_postgres || start_postgres
  fi

  if [[ "${FLEET_SKIP_BUILD:-0}" != "1" ]]; then
    log "Ensuring fleet-server is built…"
    cargo build -p fleet-server
  fi

  start_server
  start_ui

  load_dotenv
  local port="${PORT:-$API_PORT}"
  log "Waiting for services…"
  wait_http "http://127.0.0.1:${port}/health" "fleet-server" 90 || true
  if [[ "${FLEET_SKIP_UI:-0}" != "1" ]] && command -v dx >/dev/null 2>&1; then
    # dx may take longer (wasm compile); longer timeout
    wait_http "http://127.0.0.1:${UI_PORT}" "fleet-ui" 180 || true
  fi

  ok "Started."
  echo "  API  http://localhost:${port}   (health: /health)"
  echo "  UI   http://localhost:${UI_PORT}"
  echo "  logs ./scripts/fleet-dev.sh logs"
  echo "  stop ./scripts/fleet-dev.sh stop"
}

cmd_stop() {
  log "=== Fleet stop ==="
  stop_pid_file "fleet-server" "$PID_SERVER"
  stop_pid_file "fleet-ui" "$PID_UI"
  load_dotenv
  free_port "${PORT:-$API_PORT}" "fleet-server"
  free_port "$UI_PORT" "fleet-ui"
  ok "Processes stopped (Postgres left running — use './scripts/fleet-dev.sh down' to tear it down)."
}

cmd_restart() {
  log "=== Fleet restart ==="
  cmd_stop || true
  sleep 0.5
  cmd_start
}

cmd_down() {
  cmd_stop || true
  log "Stopping docker compose…"
  compose down || true
  ok "Down."
}

cmd_status() {
  ensure_env
  load_dotenv
  local port="${PORT:-$API_PORT}"
  echo "Run dir: $RUN_DIR"
  echo

  local spid upid
  spid="$(read_pid "$PID_SERVER")"
  upid="$(read_pid "$PID_UI")"

  if is_pid_alive "$spid"; then
    ok "fleet-server  running  pid=$spid  port=$port"
  else
    warn "fleet-server  not running  (pidfile=${spid:-none})"
  fi
  if is_pid_alive "$upid"; then
    ok "fleet-ui      running  pid=$upid  port=$UI_PORT"
  else
    warn "fleet-ui      not running  (pidfile=${upid:-none})"
  fi

  echo
  if compose ps 2>/dev/null | grep -q postgres; then
    compose ps
  else
    warn "docker compose postgres: not listed (run setup/start)"
  fi

  echo
  if curl -sf --max-time 2 "http://127.0.0.1:${port}/health" >/dev/null 2>&1; then
    ok "health  http://127.0.0.1:${port}/health  → OK"
    curl -s --max-time 2 "http://127.0.0.1:${port}/health" || true
    echo
  else
    warn "health  http://127.0.0.1:${port}/health  → unreachable"
  fi
  if curl -sf -o /dev/null --max-time 2 "http://127.0.0.1:${UI_PORT}" 2>/dev/null; then
    ok "ui      http://127.0.0.1:${UI_PORT}  → OK"
  else
    warn "ui      http://127.0.0.1:${UI_PORT}  → unreachable"
  fi

  echo
  echo "Logs: $LOG_SERVER"
  echo "      $LOG_UI"
}

cmd_logs() {
  mkdir_run
  local which="${1:-all}"
  case "$which" in
    server|api)
      [[ -f "$LOG_SERVER" ]] || { warn "no server log yet"; exit 0; }
      tail -n 100 -f "$LOG_SERVER"
      ;;
    ui|frontend)
      [[ -f "$LOG_UI" ]] || { warn "no ui log yet"; exit 0; }
      tail -n 100 -f "$LOG_UI"
      ;;
    all|*)
      if [[ ! -f "$LOG_SERVER" && ! -f "$LOG_UI" ]]; then
        warn "no logs yet — start services first"
        exit 0
      fi
      # Prefer multitail if present; else interleaved tail -F
      if command -v multitail >/dev/null 2>&1 && [[ -f "$LOG_SERVER" && -f "$LOG_UI" ]]; then
        multitail "$LOG_SERVER" "$LOG_UI"
      else
        touch "$LOG_SERVER" "$LOG_UI"
        tail -n 80 -F "$LOG_SERVER" "$LOG_UI"
      fi
      ;;
  esac
}

main() {
  local cmd="${1:-}"
  shift || true
  case "$cmd" in
    setup)   cmd_setup "$@" ;;
    start)   cmd_start "$@" ;;
    stop)    cmd_stop "$@" ;;
    restart) cmd_restart "$@" ;;
    status)  cmd_status "$@" ;;
    logs)    cmd_logs "$@" ;;
    down)    cmd_down "$@" ;;
    -h|--help|help|"") usage 0 ;;
    *)
      err "unknown command: $cmd"
      usage 1
      ;;
  esac
}

main "$@"
