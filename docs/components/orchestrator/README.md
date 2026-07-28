# Orchestrator / Controller

Session control plane: accepts integration inputs, commands the Sandbox Manager, drives Drivers, evaluates tool permissions, proxies MCP/secrets, and persists traces.

This folder will hold the component-level design. Until then, the system-wide
architecture and event catalogue live in:

- [`docs/architecture/README.md`](../../architecture/README.md) — layers, components, sequences
- [`docs/architecture/events.md`](../../architecture/events.md) — commands & events
- [`docs/architecture/diagrams/`](../../architecture/diagrams/) — Mermaid sources
