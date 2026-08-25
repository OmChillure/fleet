# Sandbox Manager

Creates, observes, and destroys sandboxes on hosting backends. Emits async lifecycle events (ready, OOM, node loss, archived) that the Orchestrator reacts to.

**Kubernetes hosting** is implemented as `FleetSandbox` CRs reconciled by
[`fleet-operator`](../../../crates/fleet-operator/) (see [`infra/`](../../../infra/)).
The orchestrator uses `SANDBOX_BACKEND` (`stub` / `process` / `docker` / `k8s`).
In-cluster API defaults to `k8s` (creates/deletes CRs); the operator still
turns them into pods. Host `.env` stays `stub`/`process`.

This folder will hold the component-level design. Until then, the system-wide
architecture and event catalogue live in:

- [`docs/architecture/README.md`](../../architecture/README.md) — layers, components, sequences
- [`docs/architecture/events.md`](../../architecture/events.md) — commands & events
- [`docs/architecture/diagrams/`](../../architecture/diagrams/) — Mermaid sources
