# agent-fleet-contracts

Shared **command and event** types for the Agent Fleet control plane.

- Domain model: [`docs/architecture/events.md`](../../docs/architecture/events.md)
- Outline: [`docs/architecture/events-overview.md`](../../docs/architecture/events-overview.md)

## Encoding

Structs use `serde`. The default wire format is **JSON** (UTF-8), one object per
message: envelope fields at the top level plus a typed `payload`. Transport
(Zenoh, NATS, Postgres JSONB, …) is out of scope for this crate — it only owns
the schema.

```rust
use agent_fleet_contracts::{
    BusMessage, MessageBody, Producer, SandboxFailed, SandboxFailureReason,
    SandboxFailureSeverity, SandboxId,
};

let sandbox_id = SandboxId::new();
let msg = BusMessage::new(
    Producer::SandboxManager,
    MessageBody::SandboxFailed(SandboxFailed {
        sandbox_id,
        severity: SandboxFailureSeverity::Failed,
        reason_code: SandboxFailureReason::OomKilled,
        message: "Pod OOMKilled by kubelet".into(),
        restart_count: Some(1),
        backend_raw: None,
    }),
);
// Envelope scope is filled from the payload:
assert_eq!(msg.envelope.sandbox_id, Some(sandbox_id));

let json = serde_json::to_string(&msg)?;
let back: BusMessage = serde_json::from_str(&json)?;
```

## Stability

`SCHEMA_VERSION` is currently `0.1.0`. Additive optional fields are fine;
renames/removals require a major bump and dual-read when needed.

Unknown wire labels for open enums (`Producer`, `SandboxBackend`) round-trip
as `Other(String)` without rewriting. Closed taxonomy enums (e.g.
`SessionState`) are intentionally strict in v0.1.
