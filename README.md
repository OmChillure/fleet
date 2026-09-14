# Agent Fleet

Platform for delegating development work to fleets of sandboxed coding agents,
integrated with existing tools (Jira, GitHub, Slack, …).

## Workspace crates

Rust workspace (see root `Cargo.toml`).

| Crate | Path | Role |
| --- | --- | --- |
| [`fleet-server`](crates/fleet-server/) | `crates/fleet-server` | **Unified API** — `cargo run` entrypoint |
| [`fleet-auth`](crates/fleet-auth/) | `crates/fleet-auth` | Auth + workspaces library (mounted by server) |
| [`fleet-integrations`](crates/fleet-integrations/) | `crates/fleet-integrations` | OAuth integrations (GitHub, GitLab, Trello, Asana, Slack, Notion, …) + encrypted tokens |
| [`fleet-db`](crates/fleet-db/) | `crates/fleet-db` | Postgres — migrations, pool, models, repos |
| [`fleet-ui`](crates/fleet-ui/) | `crates/fleet-ui` | Web UI (Dioxus) |
| [`agent-fleet-contracts`](crates/agent-fleet-contracts/) | `crates/agent-fleet-contracts` | Shared command/event types |
| [`fleet-operator`](crates/fleet-operator/) | `crates/fleet-operator` | Kubernetes operator for `FleetSandbox` CRs |
| [`fleet-driver`](crates/fleet-driver/) | `crates/fleet-driver` | Process worker: ACP (mock/grok) + orch heartbeat/status |
| [`fleet-sandbox-driver`](crates/fleet-sandbox-driver/) | `crates/fleet-sandbox-driver` | In-sandbox driver: per-task compose, volume provision, multi-harness ACP |
| [`fleet-host`](crates/fleet-host/) | `crates/fleet-host` | Self-hosted worker host (`pair` / `run`); outbound to the control plane |

## Local development

Install is one command, no flags:
`crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`
(`PROFILE` / `ENABLE_*` in repo-root `.env`). Do not `cargo install` around a
missing `dx` / `kind` / Playwright.

Full guide: **[`development.md`](development.md)** (Docker Compose full stack + host overrides).  
Kubernetes: **[`infra/README.md`](infra/README.md)** (kustomize + `fleet-operator`).

| Process | Command | URL |
| --- | --- | --- |
| **Full stack** | `docker compose up -d --build` | UI `http://localhost:3030`, API `:8080` |
| Backend (host) | `cargo run` | `http://localhost:8080` |
| Frontend (host) | `dx serve --package fleet-ui --port 3030` | `http://localhost:3030` |

Use the **same host** for both (prefer `localhost` in the browser) so cookie
auth works across ports.

```bash
# From agent-fleet/
cp .env.example .env
# Set DATABASE_URL (Neon / Supabase / local Postgres; include ?sslmode=require for Neon)
# JWT_SECRET must be ≥ 32 characters
# Optional: INTEGRATIONS_ENCRYPTION_KEY="$(openssl rand -hex 32)" for OAuth token AES key

# Terminal 1 — API (auth, workspaces, integrations, health)
cargo run
# → http://localhost:8080

# Terminal 2 — UI (Dioxus CLI 0.7.x matching workspace dioxus pin)
dx serve --package fleet-ui --port 3030
# → http://localhost:3030
```

### Auth + workspaces (what works today)

1. **Signup** (`/signup`) creates a user and a **default personal workspace**
   named like `{email-local}'s workspace` (or `{display_name}'s workspace`).
2. **Login** (`/login`) sets httpOnly JWT cookies, ensures a default workspace
   exists if the user has none, and redirects to:
   ```text
   /{workspace_id}/dashboard
   ```
3. **App shell** requires a live session (`GET /api/auth/me`). Unauthenticated
   visits to app routes redirect to `/login`.
4. **Sidebar** has a workspace switcher (list + create). Nav sections: Chat,
   Monitoring, Agents, Runners (self-hosted / cloud / partners), Workspace
   (Integrations, Settings). `/{workspace_id}/workers` redirects to self-hosted.
5. **Toasts** (bottom-right) for success / error across login, signup, and
   integrations. **OAuth tokens** are AES-256-GCM encrypted at rest.
6. **Chat** supports **Grok (CLI auth)** and **local Ollama**. Grok reads
   `~/.grok/auth.json` from `grok login` (same idea as Claude Code credentials);
   The Grok model is whatever `grok models` lists first on that runner (not pinned).
   Ollama: `OLLAMA_BASE_URL` (default `http://127.0.0.1:11434`).
   Chat supports every major provider the runner or control-plane key can reach
   (Grok, Claude, Ollama, OpenAI, Gemini, …; add more as demand arrives).
   Grok / Claude / Ollama run on a paired runner’s official CLI. The control
   plane still executes fleet tools (`list_agent_personas`, `launch_agent_plan`,
   `integration_query`) from that reply so Slack → GitHub → Notion works
   without tokens leaving the plane. Plan `outputs` write back when a run ends.
   Live guests
   message each other mid-run (`team_chat_post` / `team_chat_list` with `kind`,
   or `./fleet-tool.sh chat-post` / `chat-list`), share `work/`,
   and reviewers can `request_changes` to bounce work back. Guest models run on
   the paired runner's official CLI (same as chat), using the **Chat composer**
   provider. Settings Ollama /
   `FLEET_AGENT_BACKEND=grok` only select the harness when a run has no chat
   model. Without a paired runner the task stays ready; without a harness,
   sandboxes use mock.
   **Cases** (`start_case` / `observe_case`): durable backend state machine for
   long-running policies (wake loop owns status/stage; integrations optional later).
7. **Sandboxes** page lists worker sessions (`GET …/fleet/workers`) leased to a
   paired `fleet-host`. The API process is not a runner: tick enqueues a claim
   or leaves the task ready. Isolation is process / container / vm **on the
   host**. Guest is `fleet-driver`
   (`FLEET_HARNESS=grok|claude|ollama` official CLI, or `mock` ACP). In-cluster, `fleet-host` creates
   `FleetSandbox` CRs; the operator makes pods. Finished workdirs are kept 15
   minutes for SessionDetail / artifacts, then wiped. OAuth tokens never enter
   the sandbox.
8. **Runners** (`/{workspace_id}/runners/self-hosted`): add a runner, copy the install
   command and one-time pair token. The machine pairs outbound (no inbound
   port). Tick enqueues a claim for an online matching host; otherwise the
   task stays ready.
   Install: `curl -fsSL http://127.0.0.1:8080/install-worker.sh | FLEET_URL=http://127.0.0.1:8080 sh`
   then `fleet-host pair --url … --token …`. See
   [`docs/components/worker-host/README.md`](docs/components/worker-host/README.md).

| Method | Path | Notes |
| --- | --- | --- |
| `POST` | `/api/auth/signup` | Creates user + default workspace |
| `POST` | `/api/auth/login` | Cookies + `default_workspace` in body |
| `POST` | `/api/auth/logout` | Clears cookies / revokes refresh |
| `POST` | `/api/auth/refresh` | Rotates tokens |
| `GET` | `/api/auth/me` | User + workspaces (ensures default) |
| `GET` | `/api/workspaces` | List memberships (ensures default) |
| `POST` | `/api/workspaces` | Create workspace `{ "name": "…" }` |
| `GET` | `/api/workspaces/{id}/members` | List members + your role |
| `POST` | `/api/workspaces/{id}/invites` | Invite by email (`admin`/`member`); existing users join immediately |
| `GET` | `/api/workspaces/{id}/personas` | List workspace personas |
| `POST` | `/api/workspaces/{id}/personas` | Create custom persona |
| `POST` | `/api/workspaces/{id}/personas/seed-starters` | Idempotent 8 starter personas |
| `GET` | `/api/workspaces/{id}/integrations` | Catalog + OAuth connection state |
| `GET` | `/api/workspaces/{id}/integrations/{provider}/oauth/start` | Start OAuth (302 to provider) |
| `GET` | `/api/integrations/oauth/callback/{provider}` | OAuth callback → encrypt + store tokens |
| `DELETE` | `/api/workspaces/{id}/integrations/{provider}` | Disconnect + wipe secrets |
| `GET` | `/api/workspaces/{id}/chat/status` | Runner inventory (`?runner_id=`) or control-plane OpenAI/Gemini keys |
| `GET`/`POST` | `/api/workspaces/{id}/chat/conversations` | List / create saved chats |
| `GET`/`DELETE` | `/api/workspaces/{id}/chat/conversations/{cid}` | Load / delete a chat |
| `POST` | `/api/workspaces/{id}/chat` | Chat turn (persisted; fleet + integration tools) |
| `POST` | `/api/workspaces/{id}/chat/stream` | Same turn as NDJSON stream (`status`/`delta`/`action`/`done`) |
| `GET` | `/api/workspaces/{id}/fleet/workers` | Sandboxes (worker sessions) |
| `GET`/`POST` | `/api/workspaces/{id}/worker-hosts` | List / create paired worker hosts (owner; pair token once) |
| `GET`/`POST` | `/api/workspaces/{id}/inbox` | Ticket inbox (Start monitor follows connected apps and auto-launches by kind) |
| `POST` | `/api/workspaces/{id}/fleet/tasks/{task_id}/resume` | HITL resume a blocked task |
| `POST` | `/api/workspaces/{id}/fleet/sessions/{session_id}/kill` | Kill a live sandbox |
| `POST` | `/api/workspaces/{id}/fleet/sessions/{session_id}/replace` | Kill the sandbox, keep the task (new agent). Body `{ worker_profile, reason }` swaps the persona. |
| `GET` | `/api/workspaces/{id}/workforce` | Open goals (durable overnight) |
| `POST` | `/api/workspaces/{id}/workforce/goals` | Set a durable goal (case-backed) |
| `GET`/`POST` | `/api/workspaces/{id}/cases` | Durable cases (policy + backend wake loop) |
| `GET` | `/api/workspaces/{id}/cases/{case_id}` | Case detail + events |
| `POST` | `/api/workspaces/{id}/cases/{case_id}/observe` | Inject fact + wake |
| `GET` | `/api/admin/overview` | Instance counts (instance admin) |
| `GET` | `/api/admin/users` | All users (instance admin) |
| `PATCH` | `/api/admin/users/{id}` | Promote / disable (`instance_admin`, `status`) |
| `GET` | `/api/admin/workspaces` | All workspaces |
| `GET` | `/api/admin/runners` | All paired hosts |
| `GET` | `/api/admin/instance` | Health, public URLs, provider flags, staged packages |
| `GET` | `/api/admin/work` | Cross-workspace session/claim/case totals |
| `GET` | `/health` | Liveness + DB probe |

UI app routes (workspace-scoped):

| Path | Page |
| --- | --- |
| `/{workspace_id}/dashboard` | Workforce (goals, approvals, board) |
| `/{workspace_id}/chat` | New chat (runner + control-plane providers; auto-saved) |
| `/{workspace_id}/chat/{conversation_id}` | Resume a saved chat |
| `/{workspace_id}/fleet` | Fleet control plane UI |
| `/{workspace_id}/sessions` | Chat history (list / open / delete) |
| `/{workspace_id}/sessions/{session_id}` | Live worker session stream |
| `/{workspace_id}/agents` | Agents & personas (catalog + edit) |
| `/{workspace_id}/sandboxes` | Live worker sessions |
| `/{workspace_id}/cases` | Durable cases (stage + events) |
| `/{workspace_id}/cases/{case_id}` | Case detail, observe, cancel |
| `/{workspace_id}/inbox` | Ticket inbox (monitor + finish) |
| `/{workspace_id}/integrations` | OAuth connect (GitHub, GitLab, Trello, Asana, Slack, Notion, …) |
| `/admin` | Instance admin (users, all runners, health). Not workspace-scoped |
| `/{workspace_id}/settings` | Concurrent agents (default 16; instance cap 128) + members + fleet LLM fallback |
| `/{workspace_id}/runners/self-hosted` | Paired runner hosts (create + install/pair token) |
| `/{workspace_id}/runners/cloud` | Fleet Cloud / BYO attach |
| `/{workspace_id}/runners/partners` | Partner runners + trust disclaimer |
| `/{workspace_id}/workers` | Redirects to self-hosted runners |

Internal workspace crates (`fleet-orchestrator`, `fleet-bus`, `fleet-chat-md`,
`fleet-runners-view`) are listed in the root [`Cargo.toml`](Cargo.toml).

Crate docs: [`fleet-server`](crates/fleet-server/README.md),
[`fleet-auth`](crates/fleet-auth/README.md),
[`fleet-ui`](crates/fleet-ui/README.md),
[`fleet-integrations`](crates/fleet-integrations/README.md),
[`fleet-db`](crates/fleet-db/README.md),
[`fleet-driver`](crates/fleet-driver/README.md),
[`fleet-sandbox-driver`](crates/fleet-sandbox-driver/README.md).

## Docs

| Doc | Description |
| --- | --- |
| [`docs/product_vision.md`](docs/product_vision.md) | Product vision and team notes |
| [`docs/architecture/README.md`](docs/architecture/README.md) | **Orchestration architecture** (layers, components, sequences) |
| [`docs/architecture/sandbox-workspace.md`](docs/architecture/sandbox-workspace.md) | **Sandbox: harness vs workspace** (compose prototype) |
| [`docs/architecture/events-overview.md`](docs/architecture/events-overview.md) | Typical events outline (lifecycle + domains) |
| [`docs/architecture/events.md`](docs/architecture/events.md) | Field-level command & event catalogue + JSON serialization |
| [`docs/architecture/transport-zenoh.md`](docs/architecture/transport-zenoh.md) | Zenoh transport evaluation + key-space |
| [`docs/architecture/diagrams/`](docs/architecture/diagrams/) | Mermaid diagram sources |
| [`docs/components/ui/README.md`](docs/components/ui/README.md) | **Web UI** (routes, chat, toasts, theming) |
| [`docs/components/personas/`](docs/components/personas/) | **Personas** schema, API, Agents UI ([plan](docs/components/personas/plan.html)) |
| [`docs/components/auth/README.md`](docs/components/auth/README.md) | **Auth + workspaces** API and cookie model |
| [`docs/components/integrations/README.md`](docs/components/integrations/README.md) | **OAuth integrations** + credential encryption |
| [`docs/components/worker-host/README.md`](docs/components/worker-host/README.md) | Self-hosted worker hosts (create-then-pair, outbound) |
| [`docs/components/`](docs/components/) | Per-component design stubs |
| [`docs/references/`](docs/references/) | Reference projects and prior vision artifacts |
