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
#   ./scripts/human-test.sh              # launch + guide (scoped to HUMAN_TEST_SCOPE.md)
#   ./scripts/human-test.sh launch       # start Postgres + API + UI
#   ./scripts/human-test.sh guide        # scoped walkthrough (stack must be up)
#   ./scripts/human-test.sh guide-all    # full catalog (first product tour only)
#   ./scripts/human-test.sh record       # film HUMAN_TEST_SCOPE.md scenes + frames
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
  if [[ -f "${ROOT}/.run/host-urls.env" ]]; then
    # shellcheck disable=SC1091
    set -a
    . "${ROOT}/.run/host-urls.env"
    set +a
    UI_URL="${FLEET_UI_URL:-$UI_URL}"
    API_URL="${FLEET_API_URL:-$API_URL}"
  fi
  require_this_tree || {
    err "Launched stack is not this tree. Logs: ./scripts/fleet-dev.sh logs"
    exit 1
  }
  ok "UI  ${UI_URL}"
  ok "API ${API_URL}/health  git_sha=${FLEET_GIT_SHA}"
}

SCOPE_FILTER=0
SCOPE_TITLES=()

load_scope_pauses() {
  if [[ ! -f "${ROOT}/HUMAN_TEST_SCOPE.md" ]]; then
    err "HUMAN_TEST_SCOPE.md missing. Run: python3 tests/browser/surface_scope.py --write"
    exit 1
  fi
  SCOPE_FILTER=1
  SCOPE_TITLES=()
  local line
  while IFS= read -r line; do
    [[ -n "${line}" ]] && SCOPE_TITLES+=("${line}")
  done < <(python3 "${ROOT}/tests/browser/surface_scope.py" --pause-titles)
}

scope_wants() {
  local title="$1" prefix
  [[ "${SCOPE_FILTER}" -eq 0 ]] && return 0
  for prefix in "${SCOPE_TITLES[@]}"; do
    if [[ "${title}" == "${prefix}" || "${title}" == "${prefix} "* ]]; then
      return 0
    fi
  done
  return 1
}

pause_step() {
  local title="$1"
  shift
  if ! scope_wants "${title}"; then
    return 0
  fi
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
  if [[ "${SCOPE_FILTER}" -eq 1 ]]; then
    echo "    Scoped to HUMAN_TEST_SCOPE.md (this merge). guide-all = full catalog."
    echo "    After the agent films, they must review frames + .webm for bugs."
  else
    echo "    Full catalog (first product tour). For an MR use $0 (scoped)."
  fi
  echo "    Agents record the same scenes on the final merge tree: $0 record"
  open_browser "${UI_URL}/"

  pause_step "Landing  ${UI_URL}/" \
    "Hero, Get started → signup, Log in → login." || return 0

  pause_step "Signup  ${UI_URL}/signup" \
    "Display name + email@example.test + password (≥ 8)." \
    "Success goes to /login — signup does NOT log you in." || return 0

  pause_step "Login  ${UI_URL}/login" \
    "Same email/password → /{workspace-uuid}/dashboard." \
    "Workforce page: Set a goal, Open goals / Open runs / Live agents / Team board (empty is OK). In-progress cards have Watch → Live session." || return 0

  pause_step "Login wall" \
    "Log out (user menu) or a private window." \
    "Open ${UI_URL}/00000000-0000-0000-0000-000000000000/dashboard" \
    "Must redirect to /login." || return 0

  pause_step "Sidebar" \
    "Chat: New chat, Chat history." \
    "Monitoring: Workforce, Fleet, Sandboxes, Cases, Inbox." \
    "Agents: All agents, Live sessions (same route as Chat history)." \
    "Runners (expand it): Self-hosted, Cloud, Partners." \
    "Workspace (expand it): Integrations, Settings." \
    "Workers is gone from the sidebar; /{ws}/workers still redirects." || return 0

  pause_step "Workspace switcher" \
    "Open the name + ▼ control. Create a second workspace. Switch back." || return 0

  pause_step "Settings" \
    "Number of agents + min isolation (process / container / vm) + Fleet LLM (Ollama off/on). No git URL — collaboration is the shared workspace." \
    "Save. Host Grok is env-only (banner if FLEET_AGENT_BACKEND=grok)." || return 0

  pause_step "Runners / Self-hosted" \
    "Runners is a top-level sidebar section (same rank as Monitoring and Agents), children Self-hosted / Cloud / Partners. Self-hosted (/{ws}/runners/self-hosted). Empty state is No workers yet — the API is not a runner. Add worker, then paste the install command (this machine is fine). Cards are paired hosts only: name, status, last seen, jobs — click a card for the right panel (settings, providers, Apply, Remove). (this machine) only appears when you opened the UI on that box (localhost). Software: Current fleet-host version / Current fleet-driver version are semver from runner-packages.toml (same on every OS). Amber banner lists any required OS that is not on both current versions. Each card lists those versions, not file hashes. Providers list Grok / Claude / OpenAI / Private (Ollama) after heartbeat. Not installed: OS-specific copy-paste install. Private (Ollama) installed: disk free, popular list, tag picker (size / RAM when published), models directory, Unlisted name/URL. Disk-too-small refuses install. Sandboxing: Native (work root + process allow), VMs placeholder, Containers (k8s/docker knobs). Windows install needs the LAN URL, not localhost." \
    "If the command is loopback (127.0.0.1 / localhost / [::1]), the pair panel warns that another computer needs a LAN or public control-plane URL." \
    "Token panel is enough. Do not require a second machine; pairing is outbound." \
    "Stop taking jobs does not stop running work; new jobs go to another worker. No paired host = task stays ready." \
    "Workspace Settings still owns min isolation (process < container < vm). Behavior is above Providers. One Apply bar appears only when something changed and stays visible while you scroll. Sandboxing: Native work root, VMs placeholder, Containers as selects plus Advanced. Models directory is pre-filled. Stop taking jobs is the Behavior button. After Apply it says Pending apply until the box acks." \
    "Auto-update is on by default. Turn it off on a behind box to see Update now; click becomes Update requested." \
    "Each card shows a short machine id. Removed clones of a live hostname/platform stay hidden. Re-pair reclaims the same row." \
    "Remove worker asks “Remove this worker?” — Cancel keeps the card; Confirm removes it (a pending leftover stays hidden)." \
    "Click a card name to rename; Enter saves, Escape cancels." \
    "Pending card last seen never; after pair+heartbeat last seen just now (or Ns ago)." || return 0

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
    "Jira: new tab → one-time Atlassian OAuth app if needed, then Atlassian login + consent." \
    "Connected cards: set default runner / provider / model and Save. Inbox Finish uses those defaults." || return 0

  pause_step "Chat" \
    "New chat: runner → provider → model → effort. Defaults to last used if that pair is ready on the runner; otherwise the first installed/ready provider. Effort options come from that model (Grok 4.6 includes xhigh). Models come from the official CLI on that runner (no pinned third-party versions). Claude lists official aliases (sonnet/opus/haiku/fable) — never run `claude models` (that hangs). Ollama is labeled Private (Ollama) and says installed / pull a model, not logged in. After a reply, the bubble shows how long send → response took." \
    "Starting a chat must keep the selected runner. Note under the composer: Grok / Claude / Private (Ollama) run on that runner’s official CLI; OpenAI stays on the control-plane API key." \
    "Send a line only if a runner is selected and that provider is ready; else the empty composer is enough." \
    "Plain language is enough: read Slack / implement / push GitHub / write Notion. Connect apps under Integrations." || return 0

  pause_step "Fleet" \
    "Start 2-task run → Run tick (or Run until done)." \
    "Tick enqueues work_claims. A paired fleet-host runs the job. No host → task stays ready." \
    "The API is not a runner. Ticker may also tick on its own." \
    "With Fleet LLM on, live agents message each other (TEAM_CHAT.md / chat read_team_chat)." || return 0

  pause_step "Sandboxes" \
    "Same worker sessions as Fleet (status, backend, work dir)." || return 0

  pause_step "Kubernetes sandboxes" \
    "Kind/k3d API is stub (not a runner). After signup, fleet-host bootstrap-pairs; Start 2-task run + tick enqueues a claim; fleet-host creates the FleetSandbox CR." \
    "kubectl -n agent-fleet get deploy fleet-host,zenohd — cluster runner + bus." \
    "kubectl -n agent-fleet get fsb,pods — operator makes the pod. Cancel deletes the CR." \
    "SessionDetail may be empty (no pod live.jsonl pull yet)." \
    "Host API + cluster: KUBECONFIG + K8S_WORKER_API_URL a pod can reach (not 127.0.0.1)." || return 0

  pause_step "Cases" \
    "/{ws}/cases (Monitoring → Cases). New case: title + policy, Create case — lands on detail." \
    "Observe a fact (event kind observed appears). Then Cancel — status cancelled, action buttons hide." \
    "Empty list before create is OK. Events is the log. Decide now / Complete / Fail stay available until Cancel." \
    "Complete sets completed; Fail sets failed; both hide Observe / Decide now / Complete / Fail / Cancel." || return 0

  pause_step "Inbox" \
    "/{ws}/inbox — tickets from connected GitHub/Jira/Linear/Slack/Notion/GitLab/Trello." \
    "Start monitor follows every connected inbox app (checkbox on) and auto-launches by kind." \
    "Uncheck Follow every connected inbox app → Save filters: watch set pins to the current apps." \
    "Webhook URL on the page: paste into the provider. Slack/Linear HMAC uses the stored signing secret, not the URL token." \
    "Refresh now polls. Finish starts one open row by hand. A closed board item on a launched row should go done." \
    "Empty is OK until Integrations are connected. Start monitor without a Connect is 400." || return 0

  pause_step "Settings members" \
    "/{ws}/settings — Members: invite by email (member/admin)." \
    "Existing users join immediately; others join on signup or next login." \
    "Owner can change roles / remove; member cannot invite." || return 0

  pause_step "Chat history" \
    "/{ws}/sessions lists saved chats, not workers." \
    "Worker live view is /{ws}/sessions/{session_id} from a Fleet card." || return 0

  pause_step "User menu" \
    "Theme Black ↔ White (not persisted). Log out → /login." || return 0

  pause_step "Unify runners" \
    "Settings Isolation: process / container / vm. Save Container." \
    "Self-hosted: Add worker (token panel is enough). A process-only paired box must not take a container workspace." \
    "Fleet: Start 2-task run → Run tick. Tasks stay ready until a container-capable host is online. Never stub." \
    "Sandboxes: same sessions. Software line: missing fleet-driver is behind. Cloud/Partners stay placeholders." \
    "Agent drive: python3 -m unittest tests.browser.test_unify_usability (artifacts in .tmp/usability/)." || return 0

  pause_step "This merge request" \
    "Inbox: follow-connected, pin on uncheck, webhook URL, Finish on an open row." \
    "A second Finish on the same row must conflict, not start another fleet." \
    "If this list has no step for it, add one in scripts/human-test.sh and a" \
    "scene in tests/browser/record_demo.py in the same change." || return 0

  echo
  ok "Walkthrough done."
  echo "    Review: ask on Slack after the MR exists."
  echo "    Agent demo: $0 record  → .tmp/demos/*.webm + frames; review for bugs."
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
  if [[ -f "${ROOT}/.run/host-urls.env" ]]; then
    # shellcheck disable=SC1091
    set -a
    . "${ROOT}/.run/host-urls.env"
    set +a
    UI_URL="${FLEET_UI_URL:-$UI_URL}"
    API_URL="${FLEET_API_URL:-$API_URL}"
  fi
  local cmd="${1:-}"
  if [[ -n "${cmd}" ]]; then
    shift || true
  fi
  case "${cmd}" in
    "")
      load_scope_pauses
      if require_this_tree >/dev/null; then
        ok "Stack already up (this tree)."
      else
        cmd_launch
      fi
      cmd_guide
      ;;
    guide-all|walk)
      SCOPE_FILTER=0
      if require_this_tree >/dev/null; then
        ok "Stack already up (this tree)."
      else
        cmd_launch
      fi
      cmd_guide
      ;;
    launch|start) cmd_launch ;;
    guide)
      load_scope_pauses
      cmd_guide
      ;;
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
