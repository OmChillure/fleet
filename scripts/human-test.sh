#!/usr/bin/env bash
# Human testing: launch the product and walk a person through it.
# Agents record the MR demo video (do not ask the human to screen-record):
#   ./scripts/human-test.sh record [scenes…]
#
# Does NOT install packages. Missing cargo/docker/dx → run
#   crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh
# (no flags; PROFILE/ENABLE_* in repo-root .env).
#
# Usage:
#   ./scripts/human-test.sh              # launch if needed + interactive guide
#   ./scripts/human-test.sh launch       # start Postgres + API + UI
#   ./scripts/human-test.sh guide        # walkthrough only (stack must be up)
#   ./scripts/human-test.sh record […]   # agent: Playwright .webm → .tmp/demos/
#   ./scripts/human-test.sh record --list
#   ./scripts/human-test.sh status
#   ./scripts/human-test.sh stop
#   ./scripts/human-test.sh --help
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DEV="${ROOT}/scripts/fleet-dev.sh"
REC="${ROOT}/tests/browser/record_demo.py"
DOC="docs/human-testing.md"
INSTALL="crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh"
UI_URL="${FLEET_UI_URL:-http://localhost:3030}"
API_URL="${FLEET_API_URL:-http://127.0.0.1:8080}"

BLU=$'\033[0;34m'
GRN=$'\033[0;32m'
YLW=$'\033[0;33m'
RED=$'\033[0;31m'
RST=$'\033[0m'

log()  { printf '%s%s%s\n' "$BLU" "$*" "$RST"; }
ok()   { printf '%s%s%s\n' "$GRN" "$*" "$RST"; }
warn() { printf '%s%s%s\n' "$YLW" "$*" "$RST"; }
err()  { printf '%s%s%s\n' "$RED" "$*" "$RST" >&2; }

usage() {
  sed -n '2,22p' "$0" | sed 's/^# \?//'
  echo
  echo "Protocol: ${DOC}"
  exit "${1:-0}"
}

need_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    err "missing '${1}'."
    err "Install is one command, no arguments:"
    err "  ${INSTALL}"
    err "Persist PROFILE/ENABLE_* in ${ROOT}/.env — do not pass flags."
    return 1
  fi
}

port_up() {
  local port="$1"
  if command -v curl >/dev/null 2>&1; then
    curl -sf -o /dev/null --max-time 2 "http://127.0.0.1:${port}" 2>/dev/null \
      || curl -sf -o /dev/null --max-time 2 "http://127.0.0.1:${port}/health" 2>/dev/null
  else
    return 1
  fi
}

open_browser() {
  local url="$1"
  if command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$url" >/dev/null 2>&1 || true
  elif command -v open >/dev/null 2>&1; then
    open "$url" >/dev/null 2>&1 || true
  fi
}

require_this_tree() {
  python3 "${ROOT}/tests/browser/stack_identity.py"
}

cmd_launch() {
  need_cmd docker
  need_cmd cargo
  need_cmd curl
  need_cmd python3
  if ! command -v dx >/dev/null 2>&1; then
    err "dx (Dioxus CLI) is not on PATH — UI will not start."
    err "Run ${INSTALL} (no arguments; PROFILE/ENABLE_* in repo-root .env)."
    err "Do not treat Kind UI as a substitute for host dx."
    exit 1
  fi
  export FLEET_GIT_SHA
  FLEET_GIT_SHA="$(bash "${ROOT}/scripts/tree-id.sh")"
  log "Starting stack via ./scripts/fleet-dev.sh start"
  "${DEV}" start
  require_this_tree || {
    err "Launched stack is not this tree. Logs: ./scripts/fleet-dev.sh logs"
    exit 1
  }
  ok "UI  ${UI_URL}"
  ok "API ${API_URL}/health  git_sha=${FLEET_GIT_SHA}"
}

pause_step() {
  local title="$1"
  shift
  echo
  printf '%s▶ %s%s\n' "$GRN" "${title}" "$RST"
  local line
  for line in "$@"; do
    echo "    ${line}"
  done
  if [[ ! -t 0 ]]; then
    warn "stdin is not a TTY — not waiting. Re-run in a terminal for pauses."
    return 0
  fi
  read -r -p "    Enter when this checks out (or type s to skip remaining)… " ans || true
  if [[ "${ans}" == "s" || "${ans}" == "S" ]]; then
    warn "Remaining steps skipped."
    return 2
  fi
  return 0
}

cmd_guide() {
  if ! require_this_tree; then
    err "Live stack is not this tree (or is down). Run: $0 launch"
    exit 1
  fi

  echo
  ok "Human testing walkthrough  (${DOC})"
  echo "    Use ${UI_URL} (localhost, not 127.0.0.1) so cookies work."
  echo "    Throwaway accounts only: you@example.test — never real email/tokens."
  echo "    Agents record the MR video separately: $0 record"
  open_browser "${UI_URL}/"

  pause_step "Landing  ${UI_URL}/" \
    "Hero, Get started → signup, Log in → login." || return 0

  pause_step "Signup  ${UI_URL}/signup" \
    "Display name + email@example.test + password (≥ 8)." \
    "Success goes to /login — signup does NOT log you in." || return 0

  pause_step "Login  ${UI_URL}/login" \
    "Same email/password → /{workspace-uuid}/dashboard." \
    "Dashboard shows Open runs / Live agents / Team board / Tickets (empty is OK)." || return 0

  pause_step "Login wall" \
    "Log out (user menu) or a private window." \
    "Open ${UI_URL}/00000000-0000-0000-0000-000000000000/dashboard" \
    "Must redirect to /login." || return 0

  pause_step "Sidebar" \
    "Chat: New chat, Chat history." \
    "Monitoring: Dashboard, Fleet, Sandboxes, Cases, Inbox." \
    "Agents: All agents, Live sessions (same route as Chat history)." \
    "Runners (expand it): Self-hosted, Cloud, Partners." \
    "Workspace (expand it): Integrations, Settings." \
    "Workers is gone from the sidebar; /{ws}/workers still redirects." || return 0

  pause_step "Workspace switcher" \
    "Open the name + ▼ control. Create a second workspace. Switch back." || return 0

  pause_step "Settings" \
    "Number of agents + Fleet LLM (Ollama off/on). No git URL — collaboration is the shared workspace." \
    "Save. Host Grok is env-only (banner if FLEET_AGENT_BACKEND=grok)." || return 0

  pause_step "Runners / Self-hosted" \
    "Runners is a top-level sidebar section (same rank as Monitoring and Agents), children Self-hosted / Cloud / Partners. Self-hosted (/{ws}/runners/self-hosted). First card is the control plane (always #1; no software line; Runtime: SANDBOX_BACKEND; cannot remove). Add worker — paste the install command. Copy (Unix, Windows, token) must copy, including on HTTP LAN (no navigator.clipboard). Done keeps the pending worker; Cancel discards it." \
    "If the command is loopback (127.0.0.1 / localhost / [::1]), the pair panel warns that another computer needs a LAN or public control-plane URL." \
    "Token panel is enough. Do not require a second machine; pairing is outbound." \
    "Stop taking jobs does not stop running work; new jobs go to another worker or the control plane. Plane off + no remote host = task stays ready." \
    "No Native/Sandboxed checkboxes. Isolation is SANDBOX_BACKEND (shown on the plane card). Stop taking jobs is the Behavior button. Changing a setting shows Apply; after Apply it says Pending apply until the box acks. Auto-update and Max jobs stay disabled until the runner acks." \
    "Auto-update is on by default (not on the plane). Turn it off on a behind box to see Update now; click becomes Update requested." \
    "Each card shows a short machine id. Removed clones of a live hostname/platform stay hidden. Re-pair reclaims the same row." \
    "Remove worker asks “Remove this worker?” — Cancel keeps the card; Confirm removes it (a pending leftover stays hidden)." \
    "Click a card name (not Control plane) to rename; Enter saves, Escape cancels." \
    "Pending card last seen never; after pair+heartbeat last seen just now (or Ns ago). Control plane says this process, not last seen." || return 0

  pause_step "Workers" \
    "Alias: /{ws}/workers redirects to /{ws}/runners/self-hosted (same Self-hosted page)." \
    "/{ws}/runners also lands on Self-hosted. Record scene is runners (workers alias)." || return 0

  pause_step "Runners / Cloud" \
    "Runners → Cloud (/{ws}/runners/cloud). Placeholder cards for public cloud (AWS / GCP / Azure) and Fleet Cloud." \
    "Do not enter provider keys. Coming-soon / placeholder is success." || return 0

  pause_step "Runners / Partners" \
    "Runners → Partners (/{ws}/runners/partners). Trust copy first: unused capacity, Sandboxed only, verified vs open." \
    "Disclaimer + acknowledge checkbox — ack does not enable lending. Lend / Buy credits stay disabled." \
    "Do not pair a partner machine. Reading the page is success." || return 0

  pause_step "Agents" \
    "Seed starters → eight persona cards. No LLM is called here." || return 0

  pause_step "Integrations" \
    "Catalog + search. Connect is enabled for available providers (GitHub, GitLab, Jira, Trello, Asana, …)." \
    "GitHub: new tab → github.com (first time creates a local GitHub App, then install/consent)." \
    "GitLab / Trello / Asana: new tab → one-time OAuth app form if needed, then provider consent." \
    "Jira: new tab → one-time Atlassian OAuth app if needed, then Atlassian login + consent." || return 0

  pause_step "Chat" \
    "New chat: provider/model dropdown." \
    "Send a line only if Grok or Ollama is actually up; else the empty composer is enough." \
    "Plain language is enough: read Slack / implement / push GitHub / write Notion. Connect apps under Integrations." || return 0

  pause_step "Fleet" \
    "Start 2-task run → Run tick (or Run until done)." \
    "Default backend is stub — board should move. Ticker may also tick on its own." \
    "With Fleet LLM on, live agents message each other (TEAM_CHAT.md / chat read_team_chat)." || return 0

  pause_step "Sandboxes" \
    "Same worker sessions as Fleet (status, backend, work dir)." || return 0

  pause_step "Cases" \
    "/{ws}/cases — New case form (title + policy) creates a durable workflow." \
    "Open a case: events, Observe, Decide now, Cancel." \
    "Fleet blocked tasks have Resume; Sandboxes live rows have Kill." || return 0

  pause_step "Inbox" \
    "/{ws}/inbox — tickets from connected GitHub/Jira/Linear/Slack/Notion." \
    "Start monitor + Refresh now. Finish plans a DAG then launches it." \
    "Empty is OK until Integrations are connected." || return 0

  pause_step "Settings members" \
    "/{ws}/settings — Members: invite by email (member/admin)." \
    "Existing users join immediately; others join on signup or next login." \
    "Owner can change roles / remove; member cannot invite." || return 0

  pause_step "Chat history" \
    "/{ws}/sessions lists saved chats, not workers." \
    "Worker live view is /{ws}/sessions/{session_id} from a Fleet card." || return 0

  pause_step "User menu" \
    "Theme Black ↔ White (not persisted). Log out → /login." || return 0

  pause_step "This merge request" \
    "Do the new thing this change added." \
    "If this list has no step for it, add one in scripts/human-test.sh and a" \
    "scene in tests/browser/record_demo.py in the same change." || return 0

  echo
  ok "Walkthrough done."
  echo "    Review: ask on Slack after the MR exists."
  echo "    Agent demo: $0 record     → attach .tmp/demos/*.webm to the MR."
  echo "    Stop stack: $0 stop"
}

cmd_record() {
  need_cmd python3
  if [[ "${1:-}" != "--list" ]]; then
    if ! require_this_tree >/dev/null; then
      warn "This-tree stack not up — launching first."
      cmd_launch
    fi
  fi
  exec python3 "${REC}" "$@"
}

main() {
  local cmd="${1:-}"
  if [[ -n "${cmd}" ]]; then
    shift || true
  fi
  case "${cmd}" in
    ""|guide-all|walk)
      if require_this_tree >/dev/null; then
        ok "Stack already up (this tree)."
      else
        cmd_launch
      fi
      cmd_guide
      ;;
    launch|start) cmd_launch ;;
    guide)        cmd_guide ;;
    record)       cmd_record "$@" ;;
    status)       "${DEV}" status ;;
    stop)         "${DEV}" stop ;;
    -h|--help|help) usage 0 ;;
    *)
      err "unknown command: ${cmd}"
      usage 1
      ;;
  esac
}

main "$@"
