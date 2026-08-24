# Sandbox Manager

Creates, observes, and destroys sandboxes on hosting backends. Emits async lifecycle events (ready, OOM, node loss, archived) that the Orchestrator reacts to.

**Kubernetes hosting** is implemented as `FleetSandbox` CRs reconciled by
[`fleet-operator`](../../../crates/fleet-operator/) (see [`infra/`](../../../infra/)).
The orchestrator uses `SANDBOX_BACKEND` (`stub` / `process` / `docker` / `k8s`).
`k8s` creates and deletes `FleetSandbox` CRs; this operator still turns them
into pods.

This folder will hold the component-level design. Until then, the system-wide
architecture and event catalogue live in:

- [`docs/architecture/README.md`](../../architecture/README.md) — layers, components, sequences
- [`docs/architecture/events.md`](../../architecture/events.md) — commands & events
- [`docs/architecture/diagrams/`](../../architecture/diagrams/) — Mermaid sources
