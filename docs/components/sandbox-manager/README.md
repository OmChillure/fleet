# Sandbox Manager

Creates, observes, and destroys sandboxes on hosting backends. Emits async lifecycle events (ready, OOM, node loss, archived) that the Orchestrator reacts to.

**Kubernetes hosting** is implemented as `FleetSandbox` CRs reconciled by
[`fleet-operator`](../../../crates/fleet-operator/) (see [`infra/`](../../../infra/)).
The orchestrator still uses `SANDBOX_BACKEND=process` in-process; a later
`k8s` adapter should create/delete those CRs instead of exec'ing locally.

This folder will hold the component-level design. Until then, the system-wide
architecture and event catalogue live in:

- [`docs/architecture/README.md`](../../architecture/README.md) — layers, components, sequences
- [`docs/architecture/events.md`](../../architecture/events.md) — commands & events
- [`docs/architecture/diagrams/`](../../architecture/diagrams/) — Mermaid sources
