# Agent Fleet — behavior interactions

This document is an inventory of **what can actually happen** in the product as implemented: every actor, every user action, every app reaction, and the important ways those pieces collide.

It is **not** a vision doc. Marketing copy, old prototypes, and `docs/references/old-product-visions/` are out of scope except where they explain a name that still appears in the UI. If a button exists but the page is a stub, that is listed as a stub. If an API exists with no UI, that is listed as API-only.

**Identity rule used throughout:** a workspace and an orchestrator project are the same UUID. Cookie routes under `/api/workspaces/{id}/fleet/*` call the orchestrator with `project_id = workspace_id`.

---

## 1. Actors

| Actor | Who / what | What they can do |
| --- | --- | --- |
| **Anonymous visitor** | Browser, no cookies | Landing, login, signup. Unauthenticated app routes redirect to login. |
| **User** | Human with email/password | Own workspaces, chat, fleet, personas, integrations, settings. |
| **Workspace member** | User with a role on a workspace | All cookie-scoped APIs for that workspace. Today the creator is always `owner`. There is no invite, join, or role change API. |
| **Chat assistant** | Grok (xAI CLI) or local Ollama | Reply, call tools (up to 6 rounds per turn), persist history. |
| **Chat tools (control plane)** | Server-side functions the model invokes | Read fleet state, launch plans/tasks, query integrations, commit/PR on GitHub. Tokens never leave the control plane. |
| **Orchestrator tick** | Background loop + on-demand `POST …/tick` | Lock project → inbox → cancel → HITL resume → materialize → deps → spawn → stuck/kill → rollup → reap → optional external sync → wipe old workdirs. |
| **Sandbox runtime** | `stub` / `process` / `local\|shell` / `docker` / `k8s` | Spawn, signal (TERM/KILL), destroy, report state, list live handles. |
| **Process driver** | `fleet-driver` (`FLEET_HARNESS=mock\|grok`) | Heartbeat + status to `/v1`, ACP harness (mock or Grok Build). Clones `FLEET_REPO_URL` when set. |
| **Sandbox-lab driver** | `fleet-sandbox-driver` in Compose | HMAC handshake, provision volume, drive OpenCode / Pi / Codex / Grok over ACP, report on Redis. |
| **Workspace agent** | Python in the lab workspace container | Signed `provision` (git clone) and named allowlisted tools. No free shell unless `FLEET_ALLOW_RAW_EXEC`. |
| **Harness LLM** | OpenCode, Pi, Codex, Grok inside the lab image | Talk ACP to the driver; only see the job volume and allowlisted tools. |
| **Admin client** | Bearer `ADMIN_TOKEN` | `/v1/projects/*` create/config/fixture/tick/cancel/resume. |
| **Worker client** | Bearer worker session token | Heartbeat and status for its own session only. |
| **K8s operator** | `fleet-operator` | Reconcile `FleetSandbox` CR → Pod + NetworkPolicy; TTL delete; cleanup on finalizer. |
| **Kind / compose lab** | Host scripts + tests | Install, publish jobs, protocol tests, invoices dual-harness, browser e2e. |
| **Postgres** | Auth + orch stores | Users, sessions, workspaces, personas, integrations (encrypted), chat, projects/runs/tasks/sessions. |
| **Redis** | Lab bus only | `fleet:jobs` / `fleet:results` for compose-lab jobs. Not used by the main UI path. |
| **OAuth provider** | GitHub, Jira, Slack, Linear, Figma, Notion, Drive, Teams, Discord | Authorize, return code; server exchanges and encrypts tokens. |
| **External board sync** | `ExternalBoardSync` trait | After tick mutations, optionally push task status. Default is `NoopBoardSync`. |

---

## 2. Topology (who talks to whom)

```
 Browser (UI :3030)
    │ cookies (fleet_access, fleet_refresh)
    ▼
 fleet-server (:8080)
    ├── /api/auth/*              fleet-auth
    ├── /api/workspaces          fleet-auth
    ├── /api/workspaces/:id/personas
    ├── /api/workspaces/:id/integrations/*   fleet-integrations
    ├── /api/workspaces/:id/chat*            grok + ollama + tools
    ├── /api/workspaces/:id/fleet*           cookie → orch store/tick
    ├── /health
    └── fallback /v1/* /healthz              orchestrator (ADMIN / worker bearer)

 Background ticker in fleet-server
    └── every FLEET_TICK_INTERVAL: tick every open project

 Tick → SandboxRuntime
    ├── stub      in-process timer
    ├── process   fleet-host starts fleet-driver
    ├── local     bash commands.sh / fleet_worker.sh (Om path)
    ├── docker    docker run (no --rm; exit file)
    └── k8s       work_claims → fleet-host → FleetSandbox CR (operator → pod)

 Process worker/driver ──HTTP──▶ /v1/.../heartbeat|status

 Lab (separate from the UI tick; pair fleet-host for product jobs)
    publish-job → Redis → fleet-sandbox-driver → HMAC workspace-agent
                                              → ACP harness

 Kind cluster
    UI + API + Postgres + operator
    FleetSandbox CR → Pod (fleet-driver + mock-acp-agent) + NetworkPolicy
```

Secrets rule: OAuth tokens and integration credentials stay on `fleet-server`. Sandboxes receive only `FLEET_*` env, task files, and (lab) HMAC-signed tool maps.

---

## 3. Identity and session

### 3.1 Visitor (unauthenticated)

| User does | App does |
| --- | --- |
| Open `/` | Marketing landing: “Get started” → signup, “Log in” → login. Navbar: Home, Dashboard (goes to login), Log in, Sign up. |
| Open `/login` | Email + password form. Link to signup. |
| Open `/signup` | Display name (optional), email, password (≥ 8, ≤ 128). Link to login. |
| Open any `/{workspace}/…` without cookies | AppShell calls `GET /api/auth/me` → 401 → replace route with `/login`. |
| Call any cookie API without cookies | `401 not authenticated`. |
| Hit API `/` with `Accept: text/html` | Small HTML index pointing at UI `:3030`. |
| Hit API `/` as JSON | Service map of `/health`, `/api`, `/v1`. |

### 3.2 Signup

| User does | App does |
| --- | --- |
| Submit valid email + password | Create user (email unique, normalized lowercase). Create personal workspace `{local}'s workspace` (or `{display}'s workspace`) with slug `{email-slug}-{userid8}`. Role `owner`. **No session cookies.** Response: “account created — please log in”. |
| UI after 201 | Toast “Account created…”, navigate to `/login`. Login page shows success toast via `just_registered` flag. |
| Invalid email (no `@`) | `400 valid email is required`. |
| Password &lt; 8 or &gt; 128 | `400`. |
| Duplicate email | Conflict / unique violation surfaced as error toast. |

Signup does **not** log the user in.

### 3.3 Login

| User does | App does |
| --- | --- |
| Submit email + password | Lookup by normalized email. Inactive user → `401 account is not active`. Bad password → `401 invalid email or password` (same message as unknown email). |
| Success | Insert auth session (refresh hash, family id, UA, IP). Set HttpOnly cookies `fleet_access` (JWT, short TTL) and `fleet_refresh` (opaque, longer TTL). Ensure a default workspace exists. Return user + `default_workspace`. |
| UI after success | Toast “Signed in.” Navigate to `/{workspace_id}/dashboard`. |

Concurrent logins are allowed (no lock on `users`); each login is a new session row.

### 3.4 Refresh and logout

| Actor does | App does |
| --- | --- |
| UI `me()` sees expired access | Silent `POST /api/auth/refresh` with refresh cookie. |
| Refresh valid | Rotate: revoke old session row, insert new session same `family_id`, new cookies. Absolute expiry is never extended. |
| Refresh token reused (already rotated/revoked) | **Revoke the entire family.** `401 refresh token reuse detected`. |
| Refresh expired | Revoke that session. `401 refresh token expired`. |
| User clicks Log out | `POST /api/auth/logout` revokes by refresh hash, clears cookies. UI replaces route with `/login`. |
| User is deactivated later | Subsequent `me` / refresh fail with unauthorized. |

Cookies: HttpOnly, `Path=/`, `Secure` and `SameSite` from env. `SameSite=None` without `Secure` logs a warning.

### 3.5 `GET /api/auth/me`

Returns the user plus all workspaces they belong to. Side effect: if they have zero workspaces, create the default personal one.

### 3.6 AppShell gate

| Condition | UI |
| --- | --- |
| Checking | “Checking session…” |
| 401 | “Redirecting to login…” then `/login` |
| Network / 5xx | “Could not reach the API. Your session may still be valid.” + **Retry** (does not wipe cookies) |
| 200 | Sidebar + page |

---

## 4. Workspaces

| User does | App does |
| --- | --- |
| Login / me / list | Always at least one workspace. |
| Open sidebar switcher | List workspaces. Current one highlighted. |
| Click another workspace | Navigate to that workspace’s Dashboard (other pages stay on the old id until user navigates). |
| “+ Create workspace” → name → Create | `POST /api/workspaces` `{name, slug?}`. Slug = slugify(name or slug) + `-` + first 8 of new UUID. Role `owner`. Navigate to new Dashboard. |
| Empty name / name &gt; 80 | Client or `400`. |
| Slug collision | `409 workspace slug already exists`. |

There is **no** UI or API to: rename a workspace, delete a workspace, invite members, change roles, or leave.

Workspace membership is checked on every scoped route. Non-member → `403 not a member of this workspace`.

---

## 5. UI chrome (every logged-in page)

### 5.1 Sidebar

| User does | App does |
| --- | --- |
| Toggle rail | Collapse to 14-wide (brand + toggle only) or expand to 64-wide. Closing also closes the workspace menu. |
| Click brand “Agent Fleet” | Navigate to marketing `/` (still logged in). |
| Expand/collapse Chat | Sub: **New chat**, **Chat history**. |
| Expand/collapse Monitoring | Sub: **Workforce**, **Fleet**, **Sandboxes**. |
| Expand/collapse Agents | Sub: **All agents**, **Live sessions** (this last one is the *same route* as Chat history: `/{ws}/sessions`). |
| Expand/collapse Workspace | Sub: **Integrations**, **Settings**. |
| Navigate | Active section auto-expands. |

### 5.2 User footer menu

| User does | App does |
| --- | --- |
| Click avatar/name | Menu opens upward (click-away backdrop). |
| Appearance | Toggle in-memory theme Black ↔ White (`theme-black` / `theme-white`). **Not persisted.** Landing stays black. |
| Settings | `/{ws}/settings`. |
| Back to landing | `/`. |
| Log out | Logout API + `/login`. |

Toasts: success / error / info from forms and API failures.

### 5.3 Routes

| Path | Page | Auth |
| --- | --- | --- |
| `/` | Home (marketing) | Public |
| `/login` | Login | Public |
| `/signup` | Signup | Public |
| `/oauth/done` | Popup closer (no shell) | Public (query/localStorage) |
| `/:ws/dashboard` | Workforce (goals + board; writes run immediately) | Cookie |
| `/:ws/chat` | New chat | Cookie |
| `/:ws/chat/:id` | Existing thread | Cookie |
| `/:ws/fleet` | Fleet board | Cookie |
| `/:ws/sessions` | Chat history (+ “Live sessions” nav) | Cookie |
| `/:ws/sessions/:session_id` | Worker session live view | Cookie |
| `/:ws/agents` | Personas | Cookie |
| `/:ws/sandboxes` | Worker sessions list | Cookie |
| `/:ws/integrations` | Catalog + OAuth | Cookie |
| `/:ws/settings` | Git remote | Cookie |

---

## 6. Pages — user × app

### 6.1 Dashboard (Workforce)

| User does | App does |
| --- | --- |
| Open page | Title **Workforce**. Polls `GET …/fleet/overview` and `GET …/workforce`. Shows set-a-goal, open goals, team board, team chat. |
| Set a goal | `POST …/workforce/goals`. Creates a durable goal + case so it still exists tomorrow. Writes (merge, Slack, Notion) run immediately. |

### 6.2 Chat

| User does | App does |
| --- | --- |
| Open `/chat` | Load `GET …/chat/status` (providers + models). Empty composer: “How can I help?” |
| Open `/chat/{id}` | Load conversation + messages. Restore provider/model stored on the thread. |
| Pick provider | Dropdown: `grok` (if CLI auth / API key), `ollama` (if daemon + at least one model), `openai` (coming soon — toast, snap back to first available). |
| Pick model | Models from that provider (Grok: e.g. `grok-4.5`; Ollama: `/api/tags`). |
| Refresh status | Re-fetch providers (Ollama list can change after `ollama pull`). |
| Type + Enter (no Shift) or Send | If no conversation: `POST …/conversations` with title = first 72 chars, stash pending text, `replace` to `/chat/{id}`, then complete. If existing: `POST …/chat` with full history + provider + model + conversation_id. |
| Shift+Enter | Newline in draft. |
| Send while provider offline | Toast; composer disabled. |
| Send with empty model | Toast “Select a model first…”. |
| Wait for reply | “Thinking with {model}…”. On success, assistant bubble + any tool **actions** chips. On error, assistant bubble `⚠️ {message}`. |
| New chat | Navigate to `/chat` (blank). |
| History | `/{ws}/sessions`. |
| Read markdown | Client renders markdown + code highlight. |

Server complete loop:

1. Require member. Persist user message (create conversation if needed).
2. Build system prompt + live “connected integrations” snapshot.
3. Call Grok or Ollama with tool definitions.
4. If the model calls a tool: execute on the server (see §7), append tool result, repeat up to **6** rounds.
5. Persist assistant message + actions JSON. Return `{message, model, provider, actions, conversation_id}`.

Chat is per-user within a workspace (conversations belong to `user_id` + `workspace_id`).

### 6.3 Chat history (`/{ws}/sessions`)

Despite the sidebar label **Live sessions** also pointing here, this page lists **saved chats**, not worker sessions.

| User does | App does |
| --- | --- |
| Open | `GET …/chat/conversations`. |
| Click a row | `/{ws}/chat/{id}`. |
| Hover → Delete | `DELETE …/conversations/{id}`, reload list. |
| New chat / empty CTA | `/{ws}/chat`. |

### 6.4 Live worker session (`/{ws}/sessions/{session_id}`)

Reached from the **Fleet** session cards, not from Chat history.

| User does | App does |
| --- | --- |
| Open | Poll `GET …/fleet/sessions/{id}` every 1s. |
| While running | Show status, progress %, prompt.md, live.jsonl text (fallback transcript.md). |
| Session `exited` / `failed_spawn` / `killing` / failed / cancelled | Stop polling. |
| “← All sessions” | Goes to **Chat history**, not the fleet board. |

After 15 minutes terminal, tick may delete the workdir (see §8.3), so prompt/stream can go empty.

### 6.5 Fleet

On mount: `GET …/fleet` (ensures project with `max_agents=3` if missing), then tasks / runs / sessions.

| User does | App does |
| --- | --- |
| Read header | Project name, workspace id, runtime backend, repo `url@ref` or “No git repo bound — set it in Settings”. |
| **Start 2-task run** | `POST …/fleet/runs/fixture` `{kind: "chain"}` (UI does not send `diamond`). Creates accepted plan: `t1` Root → `t2` Child. Source `"ui"`. `on_task_fail = fail_run`. Does **not** tick. |
| **Run tick** | One `POST …/fleet/tick`. Show spawned / completed / runs ended. Refresh boards. |
| **Run until done** | Up to **24** ticks, 400ms apart, until every run is `completed|failed|cancelled` or max ticks. |
| **Refresh** | Re-fetch tasks/runs/sessions. |
| **Cancel latest** | Cancel the first run in the list (`POST …/runs/{id}/cancel`). Next tick drains. |
| Click a session card | `/{ws}/sessions/{session_id}` live view. |
| Look at kanban | Columns Ready / In progress / Blocked / Done / Failed from task status. |

Background ticker (if `fleet-server` is running) also ticks open projects on an interval, so the board can move **without** the user pressing Run tick.

Fixture kinds available on the API (UI only uses chain): `chain` (default), `diamond` (root → left/right → join).

### 6.6 Sandboxes

| User does | App does |
| --- | --- |
| Open / Refresh | `GET …/fleet/workers` (same sessions as fleet, product name “sandboxes”). |
| Empty | Hint: Chat launch or Fleet → Start 2-task run → Run tick. |
| See a card | Status pill, worker profile, backend, progress, short ids, timestamps, exit reason, work_dir. **No destroy/kill button.** |

Statuses: `spawning`, `healthy`, `stuck`, `killing`, `exited`, `failed_spawn`.

### 6.7 Agents & personas

Copy on the page: personas configure **future** runs. **No LLM is called here.** Worker spawn does **not** yet read persona `role_prompt`.

| User does | App does |
| --- | --- |
| Open | `GET …/personas`. |
| Seed starters | `POST …/personas/seed-starters`. Insert-if-absent 8 starters (see §9). Toast inserted/skipped. |
| New persona | Form: name, worker profile (`coding\|ops\|analysis`), description, model preference (`auto\|grok\|claude\|local` free text), max budget USD, tools CSV, default checkbox, role prompt. Create. |
| Click card | Edit form. Save → PATCH. Archive → DELETE (soft archive). Cancel closes form. |
| Filter | API supports `?status=`; UI lists whatever the default list returns (typically active). |

### 6.8 Integrations

| User does | App does |
| --- | --- |
| Open | `GET …/integrations` (catalog + connection state). |
| Search box | Client filter by name/description/category. |
| **Connect** (Available + OAuth configured) | Open popup to `GET …/integrations/{provider}/oauth/start` → 302 to provider. Button spinner. Poll up to ~3 min (90 × 1.5s) for `postMessage` or `localStorage fleet_oauth_result`. |
| Provider consents | Callback `GET /api/integrations/oauth/callback/{provider}` exchanges code, AES-GCM encrypts tokens, upserts connection, returns tiny HTML that `postMessage`s opener and closes. |
| Provider denies / error | Same HTML with error; UI toast. |
| Popup blocked / no opener | `/oauth/done` or callback page shows “close this tab”; localStorage fallback. |
| **Disconnect** | `DELETE …/integrations/{provider}`. |
| Coming soon / OAuth not configured | Button disabled or error: set `{PROVIDER}_CLIENT_ID` / `_CLIENT_SECRET`. |

### 6.9 Settings

| User does | App does |
| --- | --- |
| Open | `GET …/fleet` → fill `repo_url`, `git_ref` (default `main`). |
| Save repository | `PATCH …/fleet/config`. Empty URL clears remote (tasks will not clone). Non-empty: new tasks clone that URL at ref. |
| Invalid URL / ref without URL | `400`. |

One git remote per workspace/project. Multi-repo is not supported.

---

## 7. Chat tools (assistant × control plane × world)

The model may call these. The user never calls them directly except by asking the assistant.

### 7.1 Fleet / sandbox tools

| Tool | Effect |
| --- | --- |
| `list_sandboxes` | Sessions for this workspace. |
| `list_tasks` | Board tasks; optional `status` filter. |
| `list_runs` | Runs. |
| `get_fleet_status` | Compact: open runs, status counts, active sandboxes. |
| `launch_agent_task` | Ensure project capacity (`FLEET_MAX_AGENTS`, default 16, cap 1000). Optional `context_from` proxy fetches (markdown, no tokens) prepended to spec. Insert 1-task accepted plan. Tick up to `MAX_CHAT_TICKS` (24) so the stub/local worker can finish in-request. Return `run_id` + sandbox ids. |
| `launch_agent_plan` | Same, but a DAG (`id`, `title`, `spec`, `depends_on`, `worker_profile`, `priority`). Max **1000** tasks. Large plans only do **3** in-request ticks; background ticker drains the rest. |

Launch validation: non-empty titles ≤ 200 chars; unique plan task ids; deps must refer to ids in the same plan (orchestrator validate). Spec ` ```bash ` fences become `commands.sh` on `local`/`docker` backends.

### 7.2 Integration tools

| Tool | Effect |
| --- | --- |
| `list_integrations` | Catalog + connected flag; optional `connected_only`. |
| `get_integration` | One provider: connection, label, allowlisted ops. No secrets. |
| `integration_query` | Decrypt token on server, dispatch allowlisted op, sanitize payload, return to model. |
| `github_commit_files` | Control-plane commit (max 40 files, 512KB each). Creates branch from default if missing. Default branch `agent-fleet/changes`. |
| `github_create_pr` | Open PR after commits. |

If a provider is not connected, the tool returns an error; the system prompt tells the model to send the user to Workspace → Integrations.

### 7.3 Allowlisted proxy operations

| Provider | Operations |
| --- | --- |
| github | `list_repos`, `list_issues`, `get_issue`, `list_prs`, `get_pr`, `get_file`, `list_commits`, `put_file`, `commit_files`, `create_branch`, `create_pr`, `create_issue`, `add_comment`, `merge_pr` |
| gitlab | `list_repos`, `list_issues`, `get_issue`, `list_mrs`, `get_mr`, `get_file`, `list_commits`, `commit_files`, `create_branch`, `create_mr`, `create_issue`, `add_comment`, `merge_mr` |
| jira | `search_issues`, `get_issue`, `list_projects`, `create_issue`, `add_comment`, `list_transitions`, `transition_issue` |
| linear | `list_issues`, `get_issue`, `list_teams`, `create_issue`, `update_issue`, `add_comment` |
| slack | `list_channels`, `list_users`, `post_message`, `get_channel_history` |
| figma | `get_me`, `get_file`, `get_file_nodes`, `list_comments`, `list_team_projects`, `get_project_files` |
| notion | `search`, `get_page`, `get_database`, `query_database`, `get_block_children`, `create_page`, `append_blocks` |
| google_drive | `list_files`, `get_file`, `export_file` (readonly) |
| teams | `get_me`, `list_chats`, `list_teams`, `list_channels`, `list_chat_messages`, `send_chat_message`, `send_channel_message` |
| discord | `get_me`, `list_guilds` |
| trello | `list_boards`, `list_lists`, `list_cards`, `get_card`, `create_card`, `update_card`, `add_comment` |
| asana | `list_workspaces`, `list_projects`, `list_tasks`, `get_task`, `create_task`, `update_task`, `add_comment` |

Unknown op / disconnected / decrypt failure → tool `ok: false` with message. Results are sanitized (no token echo).

---

## 8. Orchestrator

### 8.1 Cookie fleet API (`/api/workspaces/{id}/fleet`)

All require a valid member cookie. Most call `ensure_project(workspace_id, name, max_agents=3)` first.

| Method | Path | Behavior |
| --- | --- | --- |
| GET | `/` | Project + config (`max_agents`, `repo_url`, `git_ref`) + role. Plane is not a runner. |
| PATCH | `/config` | Set/clear git remote. |
| POST | `/runs/fixture` | Insert accepted plan (`chain` or `diamond`). |
| POST | `/tick` | One locked tick. `409` if project lock held. |
| GET | `/tasks` | All tasks. |
| GET | `/runs` | All runs. |
| GET | `/sessions` | Worker sessions + backend. |
| GET | `/sessions/{id}` | Session + prompt / live.jsonl / transcript / result.json from disk. |
| GET | `/workers` | Same sessions, JSON key `sandboxes`. |
| POST | `/runs/{id}/cancel` | Set `cancel_requested_at`. |

### 8.2 Admin / worker API (`/v1`, bearer)

| Who | Method | Path | Behavior |
| --- | --- | --- | --- |
| Admin | POST | `/v1/projects` | Create project (`name`, optional `max_agents`, `repo_url`, `git_ref`). |
| Admin | GET | `/v1/projects/{id}` | Project. |
| Admin | GET/PATCH | `/v1/projects/{id}/config` | Git remote (same semantics as cookie PATCH). |
| Admin | POST | `/v1/projects/{id}/runs/fixture` | Arbitrary `WorkPlanBody` (not just chain/diamond). |
| Admin | GET | `/v1/projects/{id}/runs` | Runs. |
| Admin | POST | `/v1/projects/{id}/runs/{run}/cancel` | Cancel. |
| Admin | GET | `/v1/projects/{id}/tasks` | Filter `run_id`, `status`. |
| Admin | GET | `/v1/projects/{id}/tasks/{task}` | Full task including spec, HITL notes. |
| Admin | POST | `/v1/projects/{id}/tasks/{task}/resume` | HITL resume (`message`). `409` if task not blocked. **No UI.** |
| Admin | GET | `/v1/projects/{id}/workers` | Sessions. |
| Admin | POST | `/v1/projects/{id}/admin/tick` | Same tick as cookie. |
| Worker | POST | `…/workers/{session}/heartbeat` | `{seq, progress_pct?}`. Auth = session token. 202. |
| Worker | POST | `…/workers/{session}/status` | `{seq, status, reason?}`. 202. |
| Anyone | GET | `/healthz` | `{status: ok}` (no DB). |
| Anyone | GET | `/readyz` | DB acquire or 503. |
| Anyone | GET | `/openapi.yaml` | Spec. |

Wrong worker token → 401. Session in another project → 404.

### 8.3 Tick sequence (every tick, every open project)

1. **Advisory lock** the project (conflict if another tick holds it).
2. **Inbox** — apply queued worker reports (heartbeat progress, status).
3. **Repair** `assigned` + healthy session → `in_progress`.
4. **Cancel drain** — if run cancel requested: cancel tasks, TERM then KILL after `kill_grace_s`.
5. **HITL resume** — blocked tasks with `hitl_resume_requested_at` → `ready`.
6. **Materialize** accepted, non-cancelled, not-yet-materialized plans into board tasks.
7. **Deps** — `queued` tasks whose deps are all `done_success` → `ready`; failed dep → `done_failed`.
8. **Promote** `retry_wait` whose backoff elapsed → `ready`.
9. **Spawn** — among `ready` tasks, fill capacity (`max_agents`; sessions in `spawning|healthy|stuck|killing` hold a slot). For each assignable host: create session, materialize payload, enqueue `work_claims.spawn_session`. No host → leave ready. Failure → `failed_spawn`, task failed/retry.
10. **Stuck / completion** — stale heartbeat → `stuck` then kill; spawn timeout; task timeout; runtime `Exited` → complete or fail; retries → `retry_wait`.
11. **Rollup** — if cancel requested or (policy `fail_run` and a task failed) or all terminal: set run `completed|failed|cancelled`.
12. **Orphan reap** — destroy runtime handles with no live session.
13. **External board sync** if `auto_sync_external` (default false; noop implementation).
14. **Wipe** workdirs of `exited`/`failed_spawn` sessions older than **15 minutes** (detach git worktree first).
15. Unlock.

Background: `fleet-server` lists open project ids and ticks each on `tick_interval`.

### 8.4 Plans, tasks, runs — fields that change behavior

**Plan policy**

- `max_agents_override` — not applied from UI fixtures (null).
- `on_task_fail`: `fail_run` (default, UI fixtures) or `leave`.
- `auto_sync_external`: false in UI fixtures.

**Task**

- `depends_on` — DAG. Fan-in cap 32. Cycles rejected at insert.
- `worker_profile` — stored and shown; passed as `FLEET_WORKER_PROFILE`. Personas are not joined.
- `priority` — higher first when scheduling.
- `timeout_s`, `max_retries` — stuck/retry.
- `spec_md` — agent prompt; bash fences extracted on local/docker.
- `acceptance` — stored, not auto-evaluated.
- `external_ref` — for future board sync.

**Capacity**

- Project `max_agents` (ensure_project default 3; chat launch uses `GREATEST(existing, FLEET_MAX_AGENTS)`).
- Extra ready tasks stay `ready` until a slot frees. No Kubernetes required for queueing.

---

## 9. Personas (catalog)

Starters (seed-if-absent):

| Slug | Worker profile | Tools (labels only) |
| --- | --- | --- |
| software-engineer | coding | git, shell, editor |
| sales | ops | crm, docs |
| marketing | ops | docs, analytics |
| customer-support | ops | tickets, docs |
| analyst | analysis | sql, docs, charts |
| devops | coding | git, shell, ci |
| product-discovery | ops | docs, research |
| content-docs | ops | docs, git |

Custom personas: same fields. `is_default` is stored; spawn does not read it yet. Delete = archive (`status=archived`). Unique slug per workspace.

---

## 10. Integrations catalog

| id | UI availability | OAuth implemented | Proxy ops |
| --- | --- | --- | --- |
| github | Available | yes | yes (read+write) |
| jira | Available | yes | yes |
| slack | Available | yes | yes |
| linear | Available | yes | yes |
| teams | Available | yes | yes |
| discord | Available | yes | yes (read me/guilds) |
| figma | Available | yes | yes |
| notion | Available | yes | yes |
| google_drive | Available | yes | yes (readonly) |
| gitlab | Available | yes | yes (read+write) |
| bitbucket | Available | yes | yes |
| confluence | Available | yes | yes |
| asana | Available | yes | yes |
| trello | Available | yes | yes |

OAuth start issues a signed state JWT (user, workspace, provider). Callback verifies state, membership, provider match, then encrypts credentials with `INTEGRATIONS_CREDENTIALS_KEY`.

---

## 11. Sandbox backends

The API is not a runner. Tick assigns to a paired `fleet-host` or
leaves the task ready. Isolation is process / container / vm **on that
host**. Session `runtime_backend` is `remote`.

`fleet-host` starts `fleet-driver` (ACP). In-cluster it may create a
`FleetSandbox` CR; the operator makes the pod. Done means the guest
POSTs status and leaves `result.md` (and acceptance) where the plane
can read it.

Git isolate: if Settings has `repo_url`, the guest clones into the
session dir. No OAuth token is passed.

Cancel / kill enqueue a host signal. Finished workdirs wipe after 15
minutes.

---

## 12. Workers and drivers

### 12.1 `fleet-driver` (guest of `fleet-host`)

Same HTTP contract. Extra: provision clone from `FLEET_REPO_URL` + ref; ACP session with `FLEET_HARNESS=mock` (`mock-acp-agent`) or `grok`. Writes `prompt.md`, `live.jsonl`, `artifacts/transcript.md`, `artifacts/result.json` (what SessionDetail reads).

### 12.3 `fleet-sandbox-driver` (lab Compose)

Not started by the UI tick unless you wire it. Loop:

1. Scrub handshake secret from environ; `PR_SET_DUMPABLE=0`.
2. BLPOP `fleet:jobs` (or auto-seed from env).
3. HMAC-sign provision → workspace agent clones repo into `jobs/{job_id}/`.
4. Register `TOOLS_BY_JOB` from the signed job (harness cannot rewrite argv).
5. ACP harness: OpenCode / Pi / Codex / Grok per image.
6. Policy: deny-default tool use except allowlisted.
7. Sign result; LPUSH `fleet:results`.
8. One job then exit, or loop if `FLEET_DRIVER_LOOP`.

### 12.4 Workspace agent (lab)

| Signed action | Behavior |
| --- | --- |
| `provision` | Validate HMAC. `git clone --depth 1` (SSH via mounted deploy key, no leaked `HOME`). Write status milestones. Register tools map. |
| named tool | Argv **only** from `TOOLS_BY_JOB`. Bin must be in `ALLOWED_BINS` (python, npm, cargo, git, …). Paths must stay under jobs root. |
| unsigned tool before provision | Reject. |
| `FLEET_ALLOW_RAW_EXEC` | Legacy exec path (off by default). |

Invalid HMAC / path escape / unknown bin → signed error result.

---

## 13. Kubernetes operator

Used on kind / in-cluster. Independent of any plane env.

| Event | Operator does |
| --- | --- |
| `FleetSandbox` created | Add finalizer. Create Pod `fsb-{name}` (non-root, emptyDir, `FLEET_*` env, default image `agent-fleet-api:local` / `WORKER_IMAGE`) + NetworkPolicy. Status `Creating`. |
| Pod pending/running/ready/failed | Patch CR status phase, node, message, `started_at`. |
| `ttl_seconds` exceeded | Delete CR. |
| CR deleted | Finalizer: delete Pod + NetworkPolicy. |

**NetworkPolicy egress**

- `none` / `dns_only` (default): DNS + control-plane API only.
- `allowlist`: plus listed CIDRs (not hostnames).

Labels: `fleet.agentfleet.io/sandbox`, `session-id`, `workspace-id`, `project-id`.

In-cluster API is not a runner. Tick enqueues claims; `fleet-host` creates/deletes CRs. Sample YAML (`infra/samples/fleetsandbox.yaml`) is still valid for a cluster-admin CR without a Fleet run.

---

## 14. Health, config, and operator scripts

| Actor | Action | Result |
| --- | --- | --- |
| Anyone | `GET /health` | `{status: ok\|degraded, db: up\|down, apis: […]}`. |
| Anyone | `GET /healthz` | Orch liveness. |
| Anyone | `GET /readyz` | DB ready. |
| Browser e2e | Playwright Chromium | Landing, unauth redirect, signup/login/dashboard, sidebar pages, `/health`. Gated by `ENABLE_BROWSER_E2E`. |
| Human walkthrough | `./scripts/human-test.sh` | Starts API+UI. For an MR, only the steps that merge can break. See `docs/human-testing.md`. |
| MR demo video | Agent runs `./scripts/human-test.sh record <scenes>` | Final merge tree; affected scenes only. Playwright `.webm` in `.tmp/demos/` attached to the GitLab MR. Not committed. |
| Host | `install-full-suite-deps.sh` | Single install. Reads repo-root `.env` (`PROFILE`, `ENABLE_*`). No CLI flags. |
| Host | `run-full-suite.sh` | Cargo + protocol + host e2e + compose + invoices (+ browser if enabled). `SKIP_LLM=1` skips live LLM jobs (suite still expects protocol/invoices gates). |
| Host | `publish-job.sh` / `run-invoices-*.sh` | Per-task compose project + volume. |
| Host | `scripts/fleet-dev.sh`, `demo-orch.sh` | Local play. |

Chat providers from env: `OLLAMA_*`, Grok via `~/.grok/auth.json` and/or `GROK_API_KEY`. Integrations via `{PROVIDER}_CLIENT_ID/SECRET`, `API_PUBLIC_BASE`, `UI_PUBLIC_BASE`.

---

## 15. State machines

### 15.1 Board task

```
queued ──► ready ──► assigned ──► in_progress ──► done_success
   │          │           │              │
   │          │           ├─ blocked ──► ready
   │          │           ├─ retry_wait ► ready
   │          │           ├─ done_failed
   │          │           └─ cancelled
   ├─ done_failed (dep failed)
   └─ cancelled
```

Illegal transitions are rejected in the store.

### 15.2 Worker session

`spawning` → `healthy` → (`stuck` | `killing`) → `exited`  
or `spawning` → `failed_spawn`.

Capacity held by: `spawning`, `healthy`, `stuck`, `killing`.

### 15.3 Work run

Ingest/planning states exist on the enum; UI/chat insert at **Accepted** then **Active**, then `completed` | `failed` | `cancelled`.

### 15.4 Plan

`draft` / `ready` / `accepted` / `superseded`. Only accepted plans materialize.

### 15.5 Auth session

Active → rotated (new row, same family) → revoked. Reuse of an old refresh revokes the **family**.

### 15.6 Integration

Catalog row always exists. Connection: absent → `connected` (upsert) → disconnected (deleted/flagged). Coming-soon never starts OAuth.

### 15.7 Chat conversation

Create (empty or on first send) → messages appended → delete.

---

## 16. Cross-cutting interaction matrix

This is the “what happens when A meets B” layer.

| When | Then |
| --- | --- |
| User sends chat “launch 3 agents” | Model calls `launch_agent_plan` → plan inserted → ticks spawn up to `max_agents` → rest queue → Fleet/Sandboxes pages show sessions → SessionDetail can tail files. |
| User also presses **Run tick** | Second tick may `409` lock, or run after the chat ticks. Same board. |
| Background ticker + UI “Run until done” | Concurrent ticks on one project: one wins the lock; the other 409s (UI shows the error and may stop the loop). |
| User **Cancel latest** while chat plan is running | Next tick cancel-drains; workers get TERM/KILL; run → `cancelled`. |
| Settings git remote set after a run started | Already-spawned sessions keep their workdir. **New** spawns clone. |
| Settings git remote cleared | New tasks skip clone (prompt-only / mock). |
| Chat `context_from` GitHub while not connected | Tool error; model should tell user to Connect. |
| Chat `github_commit_files` | Commit happens on GitHub via control plane even if sandbox has no git credentials. |
| User disconnects GitHub mid-chat | Later `integration_query` fails; existing sandboxes unchanged. |
| Two workspaces, same user | Separate projects, personas, integrations, chats, boards. Sidebar switch only changes the URL prefix. |
| User logs out in one tab | Cookies cleared; other tabs fail `me` on next navigation and redirect to login. |
| Refresh reuse (stolen cookie replay) | Whole session family dead; user must log in again. |
| Persona archived | Disappears from default list; in-flight workers do not change (they never read personas today). |
| No paired host + “Run until done” | Tasks stay ready. |
| Paired host offline / spawn enqueue fail | `failed_spawn` → retry or `fail_run`. |
| Docker / k8s on the **host** full or down | Host reports error; plane does not local-spawn. |
| SessionDetail open past 15 min after exit | Tick wipe removes files; stream becomes empty. |
| HITL: worker/status or inbox sets task `blocked` | Stays blocked until `POST …/tasks/{id}/resume` (admin API). UI cannot resume. Fleet column “Blocked”. |
| Slack `post_message` via chat | Message sent with workspace Slack token; sandbox never sees the token. |
| Kind NetworkPolicy vs local `:8080` | ExtraPortMappings + overlay netpol; leaked `HOME`/`KUBECONFIG` in lab tests can accidentally hit fleet-server instead of the cluster. |
| Signup then immediately open Dashboard URL | Redirect to login (no cookies yet). |
| Open `/oauth/done` without popup | Shows close instructions; does not load AppShell. |

---

## 17. What the user **cannot** do (honest gaps)

These look like product features in vision docs or UI chrome but are **not** implemented as interactions today:

- Org-wide KPIs beyond the Workforce goal/approval/run counts.
- Invite users, roles other than owner, delete/rename workspace.
- Wire personas into spawn (prompt/model/budget/tools are stored only).
- Pick a persona when launching from chat or Fleet.
- Multi-repo projects; per-task repo override in the UI.
- Make the API a runner. `max_agents` is Settings.
- Destroy/kill a sandbox from the UI (only cancel the run).
- Resume HITL from the UI.
- Start a `diamond` fixture from the UI (API only).
- Stream chat tokens (complete is request/response).
- Persist theme or provider preference beyond the conversation row.
- Email, password reset, OAuth-login-as-user (OAuth is integrations only).
- GitLab/Bitbucket/Confluence/Asana/Trello connect.
- OpenAI chat provider (coming-soon placeholder).
- Approvals before merge (landing copy; no approval object).
- Zenoh/NATS event bus in the running server (contracts crate + docs only).
- Operator CR creation from a **host** tick unless that host is a paired `fleet-host` with KubePod.
- VM sandboxes.
- Sharing a chat or making conversations workspace-wide.

---

## 18. API cheat sheet (cookie vs bearer)

**Cookie (browser)**

```
POST /api/auth/signup|login|logout|refresh
GET  /api/auth/me
GET|POST /api/workspaces
GET|POST /api/workspaces/{ws}/personas
POST /api/workspaces/{ws}/personas/seed-starters
GET|PATCH|DELETE /api/workspaces/{ws}/personas/{id}
GET  /api/workspaces/{ws}/integrations
GET  /api/workspaces/{ws}/integrations/{provider}/oauth/start
GET  /api/integrations/oauth/callback/{provider}     # provider redirect
DELETE /api/workspaces/{ws}/integrations/{provider}
GET  /api/workspaces/{ws}/chat/status
GET|POST /api/workspaces/{ws}/chat/conversations
GET|DELETE /api/workspaces/{ws}/chat/conversations/{id}
POST /api/workspaces/{ws}/chat
GET  /api/workspaces/{ws}/fleet
PATCH /api/workspaces/{ws}/fleet/config
POST /api/workspaces/{ws}/fleet/runs/fixture
POST /api/workspaces/{ws}/fleet/tick
GET  /api/workspaces/{ws}/fleet/tasks|runs|sessions|workers
GET  /api/workspaces/{ws}/fleet/sessions/{id}
POST /api/workspaces/{ws}/fleet/runs/{id}/cancel
GET  /health
```

**Bearer admin / worker** — see §8.2.

---

## 19. Typical end-to-end stories (composed behaviors)

### A. First-time human

Signup → login → Dashboard (empty) → Settings (optional git URL) → Integrations Connect GitHub (popup) → Chat “create a PR that adds README” → model `github_commit_files` + `github_create_pr` → user sees actions + link. No sandbox required.

### B. Local fleet demo

Pair a `fleet-host` (this machine is fine) → Fleet → Start 2-task run → Run until done → kanban t1 then t2 → Sandboxes show host sessions. No host → tasks stay ready.

### C. Chat-launched agents

Chat “fan out a 5-task plan to scaffold X” → `launch_agent_plan` → first wave spawns → user watches Fleet / Sandboxes → leftover tasks drain on background ticks → cancel if needed.

### D. Process worker

Paired `fleet-host` starts `fleet-driver`. Same UI. Sessions have workdirs; SessionDetail tails `live.jsonl` until 15-minute wipe.

### E. Lab invoices (not the web app)

`install-full-suite-deps.sh` → build pinned images → `run-invoices-example.sh` → Redis job → driver + workspace agent + OpenCode/Pi → signed result. Independent of cookies and `/v1` unless you integrate it later.

### F. Kind control plane

`run-full-suite.sh` / kind overlay → UI `:3030`, API `:8080` (stub, not a runner), `fleet-host` + operator watching `FleetSandbox`. A Fleet “Start 2-task run” + tick enqueues a claim; the host creates a CR. Browser e2e clicks through marketing + auth + sidebar (does not start fixture runs today).

---

## 20. How to keep this file honest

When adding a behavior:

1. User-visible control → a row under the matching page in §6.
2. API-only → §8 or §7.
3. New actor or backend → §1 / §11 / §12.
4. New collision (tick vs chat vs cancel, etc.) → §16.
5. If it is only in a prototype or vision doc, do **not** add it here unless you mark it “not implemented” in §17.

Generated from the code on the chat + fleet + integrations + sandbox-lab tree (orchestrator tick, cookie fleet routes, Dioxus UI, lab workspace agent). Vision-only ideas are confined to §17.

How this catalog is tested: [`testing.md`](./testing.md). HTTP/auth/fleet coverage is crate integration tests (`fleet-auth`, `fleet-server`); UI chrome is `tests/browser/`.
