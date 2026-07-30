# Agent Fleet

Platform for delegating development work to fleets of sandboxed coding agents,
integrated with existing tools (Jira, GitHub, Slack, …).

## Workspace

Rust workspace (see root `Cargo.toml`).

| Crate | Path | Role |
| --- | --- | --- |
| [`fleet-server`](crates/fleet-server/) | `crates/fleet-server` | **Unified API** — `cargo run` entrypoint |
| [`fleet-auth`](crates/fleet-auth/) | `crates/fleet-auth` | Auth library (mounted by server) |
| [`fleet-db`](crates/fleet-db/) | `crates/fleet-db` | Postgres — migrations, pool, models, repos |
| [`fleet-ui`](crates/fleet-ui/) | `crates/fleet-ui` | Web UI (Dioxus) |
| [`agent-fleet-contracts`](crates/agent-fleet-contracts/) | `crates/agent-fleet-contracts` | Shared command/event types |

```bash
# Copy env and set DATABASE_URL (Neon / Supabase / local Postgres)
cp .env.example .env

# Backend — one process, all APIs (auth today; more later)
cargo run
# → http://localhost:8080

# UI (requires Dioxus CLI 0.7.x matching workspace dioxus pin)
dx serve --package fleet-ui --port 3030
# → http://localhost:3030
```

See [`crates/fleet-server/README.md`](crates/fleet-server/README.md),
[`crates/fleet-ui/README.md`](crates/fleet-ui/README.md),
[`crates/fleet-db/README.md`](crates/fleet-db/README.md), and
[`docs/components/ui/`](docs/components/ui/) for docs.

## Docs

| Doc | Description |
| --- | --- |
| [`docs/product_vision.md`](docs/product_vision.md) | Product vision and team notes |
| [`docs/architecture/README.md`](docs/architecture/README.md) | **Orchestration architecture** (layers, components, sequences) |
| [`docs/architecture/events-overview.md`](docs/architecture/events-overview.md) | Typical events outline (lifecycle + domains) |
| [`docs/architecture/events.md`](docs/architecture/events.md) | Field-level command & event catalogue + JSON serialization |
| [`docs/architecture/transport-zenoh.md`](docs/architecture/transport-zenoh.md) | Zenoh transport evaluation + key-space |
| [`docs/architecture/diagrams/`](docs/architecture/diagrams/) | Mermaid diagram sources |
| [`docs/components/fleet-ui/`](docs/components/fleet-ui/) | **Web UI** (routes, layouts, theming, status) |
| [`docs/components/`](docs/components/) | Per-component design stubs |
| [`docs/references/`](docs/references/) | Reference projects and prior vision artifacts |
