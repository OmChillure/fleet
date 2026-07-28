# Sandbox Manager

Creates, observes, and destroys sandboxes on hosting backends. Emits async lifecycle events (ready, OOM, node loss, archived) that the Orchestrator reacts to.

This folder will hold the component-level design. Until then, the system-wide
architecture and event catalogue live in:

- [`docs/architecture/README.md`](../../architecture/README.md) — layers, components, sequences
- [`docs/architecture/events.md`](../../architecture/events.md) — commands & events
- [`docs/architecture/diagrams/`](../../architecture/diagrams/) — Mermaid sources
