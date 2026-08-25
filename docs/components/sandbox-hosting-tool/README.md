# Sandbox Hosting Tool

Pluggable compute backends (Kubernetes, VMs, rootless podman / aibox-style local runners, self-hosted workers) invoked by the Sandbox Manager.

## Kubernetes (implemented)

The `k8s` backend is a **namespace-scoped operator** plus CRD:

| Piece | Path |
| --- | --- |
| CRD `FleetSandbox` | [`infra/crds/fleetsandboxes.yaml`](../../../infra/crds/fleetsandboxes.yaml) |
| Operator | [`crates/fleet-operator`](../../../crates/fleet-operator/) |
| Install (kustomize) | [`infra/`](../../../infra/) |

Each `FleetSandbox` becomes a non-root Pod running `fleet-driver` with a
NetworkPolicy that allows **DNS + control-plane API only** by default (no
general internet; secrets stay on the API). See [`infra/README.md`](../../../infra/README.md).

System-wide architecture:

- [`docs/architecture/README.md`](../../architecture/README.md) — layers, components, sequences
- [`docs/architecture/events.md`](../../architecture/events.md) — commands & events
- [`docs/architecture/diagrams/`](../../architecture/diagrams/) — Mermaid sources
