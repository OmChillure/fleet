# Agent Fleet

Platform for delegating development work to fleets of sandboxed coding agents,
integrated with existing tools (Jira, GitHub, Slack, …).

## Workspace

Rust workspace (see root `Cargo.toml`).

| Crate | Path | Role |
| --- | --- | --- |
| [`agent-fleet-contracts`](crates/agent-fleet-contracts/) | `crates/agent-fleet-contracts` | Shared command/event types (`serde` → JSON by default) |
| [`agent-fleet-driver`](driver/) | `driver` | ACP multi-harness TUI (Codex / OpenCode / Grok) |

```bash
cargo test -p agent-fleet-contracts
cargo test -p agent-fleet-driver
cargo run -p agent-fleet-driver --release
```

## Docs

| Doc | Description |
| --- | --- |
| [`docs/product_vision.md`](docs/product_vision.md) | Product vision and team notes |
| [`docs/architecture/README.md`](docs/architecture/README.md) | **Orchestration architecture** (layers, components, sequences) |
| [`docs/architecture/events-overview.md`](docs/architecture/events-overview.md) | Typical events outline (lifecycle + domains) |
| [`docs/architecture/events.md`](docs/architecture/events.md) | Field-level command & event catalogue + JSON serialization |
| [`docs/architecture/transport-zenoh.md`](docs/architecture/transport-zenoh.md) | Zenoh transport evaluation + key-space |
| [`docs/architecture/diagrams/`](docs/architecture/diagrams/) | Mermaid diagram sources |
| [`docs/components/`](docs/components/) | Per-component design stubs |
| [`docs/references/`](docs/references/) | Reference projects and prior vision artifacts |
| [`driver/README.md`](driver/README.md) | ACP driver TUI usage |
| [`driver/FLEET_TUI.md`](driver/FLEET_TUI.md) | Driver protocol notes |
