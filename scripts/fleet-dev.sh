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
#   ./scripts/fleet-dev.sh logs [all|server|ui|host]   # tail logs (default: all)
#   ./scripts/fleet-dev.sh down       # stop processes + docker compose down
#
# Env overrides:
#   FLEET_SKIP_BUILD=1   skip cargo build on start
#   FLEET_SKIP_UI=1      do not start the UI
#   FLEET_SKIP_HOST=1    do not start fleet-host (bootstrap pair + lease)
# Does not start sandbox-lab (OpenCode compose). That is a separate test harness.
#   FLEET_UI_PORT=3030   (falls back to 13030 when Kind owns :3030)
#   FLEET_API_PORT=8080  (must match .env PORT if set; falls back to 18080
#                         when Kind owns :8080 and is not this tree)
#   COMPOSE_FILE=...     docker compose file (default: docker-compose.yml)

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

RUN_DIR="${FLEET_RUN_DIR:-$ROOT/.run}"
LOG_DIR="$RUN_DIR/logs"
PID_SERVER="$RUN_DIR/fleet-server.pid"
PID_UI="$RUN_DIR/fleet-ui.pid"
PID_HOST="$RUN_DIR/fleet-host.pid"
LOG_SERVER="$LOG_DIR/fleet-server.log"
LOG_UI="$LOG_DIR/fleet-ui.log"
LOG_HOST="$LOG_DIR/fleet-host.log"

COMPOSE_FILE="${COMPOSE_FILE:-$ROOT/docker-compose.yml}"
UI_PORT="${FLEET_UI_PORT:-3030}"
API_PORT="${FLEET_API_PORT:-8080}"
HOST_API_FALLBACK="${FLEET_HOST_API_FALLBACK:-18080}"
HOST_UI_FALLBACK="${FLEET_HOST_UI_FALLBACK:-13030}"
URLS_FILE="${RUN_DIR}/host-urls.env"

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
    err "Run crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh"
    err "(no arguments; PROFILE/ENABLE_* in repo-root .env). Do not patch around it."
    return 1
  fi
}

mkdir_run() {
  mkdir -p "$RUN_DIR" "$LOG_DIR" "$ROOT/data"
}

compose() {
  export_git_sha
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

# Kind extraPortMappings listen on the host but often have no host pid
# (lsof/fuser empty). ss/curl still see them.
port_is_open() {
  local port="$1"
  if [[ -n "$(port_pids "$port")" ]]; then
    return 0
  fi
  if command -v ss >/dev/null 2>&1 && ss -ltn 2>/dev/null | grep -qE ":${port}[[:space:]]"; then
    return 0
  fi
  if command -v curl >/dev/null 2>&1; then
    if curl -sf -o /dev/null --max-time 1 "http://127.0.0.1:${port}/health" 2>/dev/null \
      || curl -sf -o /dev/null --max-time 1 "http://127.0.0.1:${port}" 2>/dev/null; then
      return 0
    fi
  fi
  return 1
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

# dx answers :3030 before wasm/public sha rebuild. Port-up is not this tree.
wait_this_tree() {
  local mode="${1:-}"
  local attempts="${2:-180}"
  local -a args=()
  if [[ "$mode" == "api-only" ]]; then
    args+=(--api-only)
  fi
  export_git_sha
  local i
  log "Waiting for stack identity (${FLEET_GIT_SHA})…"
  for i in $(seq 1 "$attempts"); do
    if python3 "${ROOT}/tests/browser/stack_identity.py" "${args[@]}" >/dev/null 2>&1; then
      ok "  stack is this tree"
      return 0
    fi
    sleep 1
  done
  python3 "${ROOT}/tests/browser/stack_identity.py" "${args[@]}" || true
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

  if ! grep -qE '^JWT_SECRET=.{32,}' "$ROOT/.env"; then
    err "JWT_SECRET must be ≥ 32 characters (set it in .env). Refusing to start."
    return 1
  fi

  if ! grep -qE '^FLEET_BOOTSTRAP_SECRET=' "$ROOT/.env"; then
    echo "FLEET_BOOTSTRAP_SECRET=dev-bootstrap-secret-change-me" >>"$ROOT/.env"
    warn "Appended FLEET_BOOTSTRAP_SECRET for local fleet-host bootstrap"
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
  need_cmd dx

  start_postgres

  if [[ "${FLEET_SKIP_BUILD:-0}" != "1" ]]; then
    export_git_sha
    log "Building fleet-server + fleet-host (FLEET_GIT_SHA=${FLEET_GIT_SHA}; first time can take a while)…"
    cargo build -p fleet-server -p fleet-host
    ok "  fleet-server + fleet-host built"
  else
    warn "Skipping cargo build (FLEET_SKIP_BUILD=1)"
  fi
  bash "$ROOT/scripts/check-worker-dist.sh" || true

  ok "Setup complete."
  echo "  .env          → $ROOT/.env"
  echo "  Postgres      → localhost:5433 (user/db: fleet)"
  echo "  DATA_DIR      → $ROOT/data"
  echo "  Run dir/logs  → $RUN_DIR"
  echo
  echo "Next: ./scripts/fleet-dev.sh start"
}

export_git_sha() {
  # Always this worktree. A leftover FLEET_GIT_SHA in the environment must not
  # bake an old value into fleet-server (compile-time option_env!).
  FLEET_GIT_SHA="$(bash "${ROOT}/scripts/tree-id.sh")"
  export FLEET_GIT_SHA
}

# Kind extraPortMappings often own :8080/:3030. Host human-test then cannot
# bind. If those ports are up and are not this tree, move the host stack to
# 18080/13030. Explicit FLEET_API_PORT / FLEET_UI_PORT always win.
choose_host_ports() {
  local want api_busy ui_busy
  want="$(bash "${ROOT}/scripts/tree-id.sh")"
  mkdir -p "$RUN_DIR"

  if [[ -n "${FLEET_API_PORT:-}" || -n "${FLEET_UI_PORT:-}" ]]; then
    API_PORT="${FLEET_API_PORT:-$API_PORT}"
    UI_PORT="${FLEET_UI_PORT:-$UI_PORT}"
  else
    if port_is_open "$API_PORT" || port_is_open "$UI_PORT"; then
      if ! env FLEET_API_URL="http://127.0.0.1:${API_PORT}" \
           FLEET_UI_URL="http://127.0.0.1:${UI_PORT}" \
           python3 "${ROOT}/tests/browser/stack_identity.py" >/dev/null 2>&1; then
        warn ":${API_PORT}/:${UI_PORT} are up but not this tree (${want})."
        warn "Starting host stack on :${HOST_API_FALLBACK}/:${HOST_UI_FALLBACK} (Kind can keep 8080/3030)."
        API_PORT="$HOST_API_FALLBACK"
        UI_PORT="$HOST_UI_FALLBACK"
      fi
    fi
  fi

  export FLEET_API_PORT="$API_PORT"
  export FLEET_UI_PORT="$UI_PORT"
  export PORT="$API_PORT"
  export FLEET_API_URL="http://127.0.0.1:${API_PORT}"
  export FLEET_UI_URL="http://localhost:${UI_PORT}"
  {
    echo "FLEET_API_PORT=${API_PORT}"
    echo "FLEET_UI_PORT=${UI_PORT}"
    echo "PORT=${API_PORT}"
    echo "FLEET_API_URL=${FLEET_API_URL}"
    echo "FLEET_UI_URL=${FLEET_UI_URL}"
  } >"$URLS_FILE"
}

write_ui_git_sha() {
  local dest="${ROOT}/crates/fleet-ui/public/fleet-git-sha.txt"
  mkdir -p "$(dirname "${dest}")"
  printf '%s\n' "${FLEET_GIT_SHA}" >"${dest}"
}

start_server() {
  load_dotenv
  export_git_sha
  # choose_host_ports may have remapped off Kind :8080. .env PORT must not win.
  local port="${FLEET_API_PORT:-${PORT:-$API_PORT}}"
  if [[ -n "$(port_pids "$port")" ]]; then
    if python3 "${ROOT}/tests/browser/stack_identity.py"; then
      warn "Port $port already in use and /health matches this tree — not starting"
      return 0
    fi
    err "Port $port is in use but is not this checkout (Kind extraPortMappings?)."
    err "Free :8080/:3030 or rebuild images from this commit. See tests/browser/stack_identity.py"
    exit 1
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
    FLEET_GIT_SHA="${FLEET_GIT_SHA}" \
    PORT="$port" \
    HOST="${HOST:-0.0.0.0}" \
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
  need_cmd dx || exit 1
  export_git_sha
  write_ui_git_sha
  if [[ -n "$(port_pids "$UI_PORT")" ]]; then
    if python3 "${ROOT}/tests/browser/stack_identity.py"; then
      warn "Port $UI_PORT already in use and UI/API match this tree — not starting"
      return 0
    fi
    err "Port $UI_PORT is in use but is not this checkout (old Kind UI?)."
    err "Free :3030 or rebuild agent-fleet-ui:local from this commit."
    exit 1
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
    FLEET_GIT_SHA="${FLEET_GIT_SHA}" \
    dx serve --package fleet-ui --port "$UI_PORT" --addr "${UI_ADDR:-$HOST}" \
    >>"$LOG_UI" 2>&1 &
  echo $! >"$PID_UI"
  ok "  fleet-ui pid $(read_pid "$PID_UI")  log → $LOG_UI"
}

start_host() {
  if [[ "${FLEET_SKIP_HOST:-0}" == "1" ]]; then
    warn "Skipping fleet-host (FLEET_SKIP_HOST=1)"
    return 0
  fi
  load_dotenv
  local pid
  pid="$(read_pid "$PID_HOST")"
  if is_pid_alive "$pid"; then
    warn "fleet-host already running (pid $pid)"
    return 0
  fi
  local port="${FLEET_API_PORT:-${PORT:-$API_PORT}}"
  mkdir -p "$RUN_DIR/host"
  log "Starting fleet-host (bootstrap pair + lease)…"
  nohup env \
    CARGO_TERM_COLOR=never \
    FLEET_GIT_SHA="${FLEET_GIT_SHA}" \
    FLEET_HOST_CONFIG_DIR="$RUN_DIR/host" \
    FLEET_CONTROL_PLANE_URL="http://127.0.0.1:${port}" \
    FLEET_BOOTSTRAP_SECRET="${FLEET_BOOTSTRAP_SECRET:-}" \
    cargo run -p fleet-host -- run \
    >>"$LOG_HOST" 2>&1 &
  echo $! >"$PID_HOST"
  ok "  fleet-host pid $(read_pid "$PID_HOST")  log → $LOG_HOST"
}

cmd_start() {
  log "=== Fleet start ==="
  ensure_env
  need_cmd cargo
  need_cmd curl
  if [[ "${FLEET_SKIP_UI:-0}" != "1" ]]; then
    need_cmd dx || exit 1
  fi
  choose_host_ports

  # Postgres first
  if ! compose ps --status running 2>/dev/null | grep -q postgres; then
    start_postgres
  else
    wait_postgres || start_postgres
  fi

  if [[ "${FLEET_SKIP_BUILD:-0}" != "1" ]]; then
    export_git_sha
    log "Ensuring fleet-server + fleet-host are built (FLEET_GIT_SHA=${FLEET_GIT_SHA})…"
    cargo build -p fleet-server -p fleet-host
  fi
  bash "$ROOT/scripts/check-worker-dist.sh" || true

  start_server
  start_ui

  load_dotenv
  local port="${FLEET_API_PORT:-${PORT:-$API_PORT}}"
  log "Waiting for services…"
  wait_http "http://127.0.0.1:${port}/health" "fleet-server" 90 || {
    err "fleet-server did not become ready. Logs: ./scripts/fleet-dev.sh logs"
    exit 1
  }
  if [[ "${FLEET_SKIP_UI:-0}" != "1" ]]; then
    # dx may take longer (wasm compile); longer timeout
    wait_http "http://127.0.0.1:${UI_PORT}" "fleet-ui" 180 || {
      err "fleet-ui did not become ready. Logs: ./scripts/fleet-dev.sh logs"
      exit 1
    }
  fi
  if [[ "${FLEET_SKIP_UI:-0}" == "1" ]]; then
    wait_this_tree api-only 90 || {
      err "Started API is not this tree. See tests/browser/stack_identity.py"
      exit 1
    }
  else
    wait_this_tree 180 || {
      err "Started ports are not this tree. See tests/browser/stack_identity.py"
      exit 1
    }
  fi

  start_host

  ok "Started."
  echo "  API  http://localhost:${port}   (health: /health)"
  echo "  UI   http://localhost:${UI_PORT}"
  echo "  host fleet-host (pairs after the first workspace exists)"
  echo "  logs ./scripts/fleet-dev.sh logs"
  echo "  stop ./scripts/fleet-dev.sh stop"
}

cmd_stop() {
  log "=== Fleet stop ==="
  stop_pid_file "fleet-host" "$PID_HOST"
  stop_pid_file "fleet-server" "$PID_SERVER"
  stop_pid_file "fleet-ui" "$PID_UI"
  load_dotenv
  if [[ -f "$URLS_FILE" ]]; then
    # shellcheck disable=SC1090
    . "$URLS_FILE"
    API_PORT="${FLEET_API_PORT:-$API_PORT}"
    UI_PORT="${FLEET_UI_PORT:-$UI_PORT}"
  fi
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

  local spid upid hpid
  spid="$(read_pid "$PID_SERVER")"
  upid="$(read_pid "$PID_UI")"
  hpid="$(read_pid "$PID_HOST")"

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
  if is_pid_alive "$hpid"; then
    ok "fleet-host    running  pid=$hpid"
  else
    warn "fleet-host    not running  (pidfile=${hpid:-none})"
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
  echo "      $LOG_HOST"
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
    host|fleet-host)
      [[ -f "$LOG_HOST" ]] || { warn "no fleet-host log yet"; exit 0; }
      tail -n 100 -f "$LOG_HOST"
      ;;
    all|*)
      if [[ ! -f "$LOG_SERVER" && ! -f "$LOG_UI" && ! -f "$LOG_HOST" ]]; then
        warn "no logs yet — start services first"
        exit 0
      fi
      # Prefer multitail if present; else interleaved tail -F
      if command -v multitail >/dev/null 2>&1 && [[ -f "$LOG_SERVER" && -f "$LOG_UI" ]]; then
        multitail "$LOG_SERVER" "$LOG_UI" "$LOG_HOST"
      else
        touch "$LOG_SERVER" "$LOG_UI" "$LOG_HOST"
        tail -n 80 -F "$LOG_SERVER" "$LOG_UI" "$LOG_HOST"
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
