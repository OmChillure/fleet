# Agent Fleet

Platform for delegating development work to fleets of sandboxed coding agents,
integrated with existing tools (Jira, GitHub, Slack, …).

## Workspace crates

Rust workspace (see root `Cargo.toml`).

| Crate | Path | Role |
| --- | --- | --- |
| [`fleet-server`](crates/fleet-server/) | `crates/fleet-server` | **Unified API** — `cargo run` entrypoint |
| [`fleet-auth`](crates/fleet-auth/) | `crates/fleet-auth` | Auth + workspaces library (mounted by server) |
| [`fleet-db`](crates/fleet-db/) | `crates/fleet-db` | Postgres — migrations, pool, models, repos |
| [`fleet-ui`](crates/fleet-ui/) | `crates/fleet-ui` | Web UI (Dioxus) |
| [`fleet-driver`](crates/fleet-driver/) | `crates/fleet-driver` | In-sandbox ACP harness driver |
| [`agent-fleet-contracts`](crates/agent-fleet-contracts/) | `crates/agent-fleet-contracts` | Shared command/event types (`serde` → JSON by default) |

## Local development

| Process | Command | URL |
| --- | --- | --- |
| Backend | `cargo run` | `http://localhost:8080` |
| Frontend | `dx serve --package fleet-ui --port 3030` | `http://localhost:3030` |

Use the **same host** for both web processes (prefer `localhost` in the browser) so cookie
auth works across ports.

```bash
# From agent-fleet/
cp .env.example .env
# Set DATABASE_URL (Neon / Supabase / local Postgres; include ?sslmode=require for Neon)
# JWT_SECRET must be ≥ 32 characters

# Terminal 1 — API (auth, workspaces, health)
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
4. **Sidebar** has a workspace switcher (list + create). Nav links stay under
   the current workspace id.

| Method | Path | Notes |
| --- | --- | --- |
| `POST` | `/api/auth/signup` | Creates user + default workspace |
| `POST` | `/api/auth/login` | Cookies + `default_workspace` in body |
| `POST` | `/api/auth/logout` | Clears cookies / revokes refresh |
| `POST` | `/api/auth/refresh` | Rotates tokens |
| `GET` | `/api/auth/me` | User + workspaces (ensures default) |
| `GET` | `/api/workspaces` | List memberships (ensures default) |
| `POST` | `/api/workspaces` | Create workspace `{ "name": "…" }` |
| `GET` | `/health` | Liveness + DB probe |

UI app routes (workspace-scoped):

| Path | Page |
| --- | --- |
| `/{workspace_id}/dashboard` | Dashboard |
| `/{workspace_id}/sessions` | Sessions |
| `/{workspace_id}/agents` | Agents |
| `/{workspace_id}/sandboxes` | Sandboxes |
| `/{workspace_id}/settings` | Settings |

Crate docs: [`fleet-server`](crates/fleet-server/README.md),
[`fleet-auth`](crates/fleet-auth/README.md),
[`fleet-ui`](crates/fleet-ui/README.md),
[`fleet-db`](crates/fleet-db/README.md),
[`fleet-driver`](crates/fleet-driver/README.md).

## fleet-driver

In-sandbox ACP driver lab. **Everything driver-related lives under** [`crates/fleet-driver/`](crates/fleet-driver/) (`src/`, `agents/`, `sandbox-lab/`). Architecture: [`sandbox-workspace.md`](docs/architecture/sandbox-workspace.md).

### What’s in place

| Area | Status |
| --- | --- |
| **Per-task sandbox** | Each job gets its own Compose project + **volume** + workspace + redis + driver. No shared `/workspace` across tasks. |
| **Harnesses** | `opencode`, `pi` ([pi.dev](https://pi.dev) via `pi-acp`), plus `codex` / `grok` images |
| **Named tools** | Agent runs project commands only via `fleet-wsexec` (workspace allowlist) |
| **Permissions** | `auto_approve=false`; path-scoped edits (no `"*"`); folder prefixes gate writes |
| **Local models** | Same `OPENCODE_*` / `MODEL` for OpenCode and Pi (LM Studio OpenAI-compatible) |
| **Output** | Quiet by default; compact result + truncated transcript |

### Tests (no LLM)

```bash
cargo test -p fleet-driver
./crates/fleet-driver/sandbox-lab/scripts/run-protocol-tests.sh /tmp/fleet-sandbox-tests.log
```

### Invoices e2e (Docker + LLM)

**One task = one sandbox.** Driver clones [123-invoices](https://github.com/eugenepyvovarov/123-invoices), agent makes a small change, runs check/migrate/targeted test (full suite optional; timeout OK). Git branches are **not** created by the driver.

**Env** — `crates/fleet-driver/sandbox-lab/.env` (from `.env.example`):

```bash
OPENCODE_PROVIDER_ID=lmstudio
OPENCODE_BASE_URL=http://host.docker.internal:1234/v1   # must reach LM Studio from the driver container
OPENCODE_API_KEY=lm-studio
MODEL=lmstudio/google/gemma-4-e4b
SSH_KEY_HOST_PATH=/tmp/fleet-lab-ssh/id_ed25519
```

| Job | Harness | Task | Edit allowlist |
| --- | --- | --- | --- |
| `job.123-invoices.opencode.json` | opencode | `app/fleet_marker.py` + smoke test | `app/`, `tests/` |
| `job.123-invoices.pi.json` | pi | `Animal.notes` + makemigrations + test | `invoices/`, `tests/` |

```bash
# once: harness images (opencode, pi, …)
./crates/fleet-driver/agents/build.sh

cd crates/fleet-driver/sandbox-lab
cp -n .env.example .env

# single task (quiet)
./scripts/run-invoices-example.sh config/job.123-invoices.opencode.json

# two tasks = two sandboxes, sequential
./scripts/run-invoices-dual.sh

# more detail
VERBOSE=1 FULL_TRANSCRIPT=1 ./scripts/run-invoices-example.sh config/job.123-invoices.pi.json
# KEEP_SANDBOX=1  — leave volume/containers after run
```

Details: [`crates/fleet-driver/README.md`](crates/fleet-driver/README.md).

## Docs

| Doc | Description |
| --- | --- |
| [`docs/product_vision.md`](docs/product_vision.md) | Product vision and team notes |
| [`docs/architecture/README.md`](docs/architecture/README.md) | **Orchestration architecture** (layers, components, sequences) |
| [`docs/architecture/sandbox-workspace.md`](docs/architecture/sandbox-workspace.md) | **Sandbox: harness vs workspace** (driver lab topology) |
| [`docs/architecture/events-overview.md`](docs/architecture/events-overview.md) | Typical events outline (lifecycle + domains) |
| [`docs/architecture/events.md`](docs/architecture/events.md) | Field-level command & event catalogue + JSON serialization |
| [`docs/architecture/transport-zenoh.md`](docs/architecture/transport-zenoh.md) | Zenoh transport evaluation + key-space |
| [`docs/architecture/diagrams/`](docs/architecture/diagrams/) | Mermaid diagram sources |
| [`docs/components/ui/README.md`](docs/components/ui/README.md) | **Web UI** (routes, layouts, theming, status) |
| [`docs/components/auth/README.md`](docs/components/auth/README.md) | **Auth + workspaces** API and cookie model |
| [`docs/components/`](docs/components/) | Per-component design stubs |
| [`docs/references/`](docs/references/) | Reference projects and prior vision artifacts |
