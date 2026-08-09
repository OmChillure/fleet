# Agent Fleet

Platform for delegating development work to fleets of sandboxed coding agents,
integrated with existing tools (Jira, GitHub, Slack, …).

## Workspace crates

Rust workspace (see root `Cargo.toml`).

| Crate | Path | Role |
| --- | --- | --- |
| [`fleet-server`](crates/fleet-server/) | `crates/fleet-server` | **Unified API** — `cargo run` entrypoint |
| [`fleet-auth`](crates/fleet-auth/) | `crates/fleet-auth` | Auth + workspaces library (mounted by server) |
| [`fleet-integrations`](crates/fleet-integrations/) | `crates/fleet-integrations` | OAuth integrations (GitHub, Slack, Notion, …) + encrypted tokens |
| [`fleet-db`](crates/fleet-db/) | `crates/fleet-db` | Postgres — migrations, pool, models, repos |
| [`fleet-ui`](crates/fleet-ui/) | `crates/fleet-ui` | Web UI (Dioxus) |
| [`agent-fleet-contracts`](crates/agent-fleet-contracts/) | `crates/agent-fleet-contracts` | Shared command/event types |
| [`fleet-operator`](crates/fleet-operator/) | `crates/fleet-operator` | Kubernetes operator for `FleetSandbox` CRs |
| [`fleet-driver`](crates/fleet-driver/) | `crates/fleet-driver` | In-sandbox ACP harness driver (lab; see crate README) |

## Local development

Full guide: **[`development.md`](development.md)** (Docker Compose full stack + host overrides).  
Kubernetes: **[`infra/README.md`](infra/README.md)** (kustomize + `fleet-operator`).

| Process | Command | URL |
| --- | --- | --- |
| **Full stack** | `docker compose up -d --build` | UI `http://localhost:3030`, API `:8080` |
| Backend (host) | `cargo run` | `http://localhost:8080` |
| Frontend (host) | `dx serve --package fleet-ui --port 3030` | `http://localhost:3030` |

Use the **same host** for both web processes (prefer `localhost` in the browser) so cookie
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
   Monitoring, Agents, Workspace. Links stay under the current workspace id.
5. **Toasts** (bottom-right) for success / error across login, signup, and
   integrations. **OAuth tokens** are AES-256-GCM encrypted at rest.

| Method | Path | Notes |
| --- | --- | --- |
| `POST` | `/api/auth/signup` | Creates user + default workspace |
| `POST` | `/api/auth/login` | Cookies + `default_workspace` in body |
| `POST` | `/api/auth/logout` | Clears cookies / revokes refresh |
| `POST` | `/api/auth/refresh` | Rotates tokens |
| `GET` | `/api/auth/me` | User + workspaces (ensures default) |
| `GET` | `/api/workspaces` | List memberships (ensures default) |
| `POST` | `/api/workspaces` | Create workspace `{ "name": "…" }` |
| `GET` | `/api/workspaces/{id}/personas` | List workspace personas |
| `POST` | `/api/workspaces/{id}/personas` | Create custom persona |
| `POST` | `/api/workspaces/{id}/personas/seed-starters` | Idempotent 8 starter personas |
| `GET` | `/api/workspaces/{id}/integrations` | Catalog + OAuth connection state |
| `GET` | `/api/workspaces/{id}/integrations/{provider}/oauth/start` | Start OAuth (302 to provider) |
| `GET` | `/api/integrations/oauth/callback/{provider}` | OAuth callback → encrypt + store tokens |
| `DELETE` | `/api/workspaces/{id}/integrations/{provider}` | Disconnect + wipe secrets |
| `GET` | `/health` | Liveness + DB probe |

UI app routes (workspace-scoped):

| Path | Page |
| --- | --- |
| `/{workspace_id}/dashboard` | Dashboard |
| `/{workspace_id}/chat` | Chat (composer + messages) |
| `/{workspace_id}/fleet` | Fleet control plane UI |
| `/{workspace_id}/sessions` | Sessions / chat history |
| `/{workspace_id}/agents` | Agents & personas (catalog + edit) |
| `/{workspace_id}/sandboxes` | Sandboxes |
| `/{workspace_id}/integrations` | OAuth connect (GitHub, Slack, Notion, …) |
| `/{workspace_id}/settings` | Settings |

Crate docs: [`fleet-server`](crates/fleet-server/README.md),
[`fleet-auth`](crates/fleet-auth/README.md),
[`fleet-ui`](crates/fleet-ui/README.md),
[`fleet-integrations`](crates/fleet-integrations/README.md),
[`fleet-db`](crates/fleet-db/README.md),
[`fleet-driver`](crates/fleet-driver/README.md).

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
| [`docs/components/ui/README.md`](docs/components/ui/README.md) | **Web UI** (routes, chat, toasts, theming) |
| [`docs/components/personas/`](docs/components/personas/) | **Personas** schema, API, Agents UI ([plan](docs/components/personas/plan.html)) |
| [`docs/components/auth/README.md`](docs/components/auth/README.md) | **Auth + workspaces** API and cookie model |
| [`docs/components/integrations/README.md`](docs/components/integrations/README.md) | **OAuth integrations** + credential encryption |
| [`docs/components/`](docs/components/) | Per-component design stubs |
| [`docs/references/`](docs/references/) | Reference projects and prior vision artifacts |
