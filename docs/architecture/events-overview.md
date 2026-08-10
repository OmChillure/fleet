# Events overview — what we need and why

Companion to the field-level catalogue in [`events.md`](./events.md).
This page is the **outline**: which messages exist, who produces/consumes
them, and which lifecycle phase they belong to. Payload shapes are sketched
here; full field lists live in the catalogue.

Status: **draft**.

---

## Mental model

Everything interesting in Agent Fleet is either:

| Kind | Meaning | Examples |
| --- | --- | --- |
| **Command** | Imperative “please do X” | create sandbox, start turn, allow tool call |
| **Event** | Fact “X happened” | sandbox OOM’d, agent proposed a tool call, turn completed |
| **Stream chunk** | High-volume partial fact | token deltas, trace frames |

Commands may get a cheap transport ack, but **long-running outcomes are always
events**. Sandbox death has no caller waiting on an RPC; interactive Slack
replies are continuous; tool-call permissions are multi-party. That is why
the control plane is event-shaped. See architecture
[§4](./README.md#4-event-driven-control-plane).

```
  Integrations ──events──▶ Orchestrator ──commands──▶ Sandbox Manager
       ▲                        │  ▲                      │
       │                        │  │                      │ events
       │                   commands│                 (ready/fail/…)
       │                        ▼  │                      │
       │                     Driver ◀─────────────────────┘
       │                        │
       └──── agent messages ────┘     (+ Postgres durable log)
```

---

## Common envelope (every message)

| Field | Purpose |
| --- | --- |
| `message_id` | Idempotency / dedupe |
| `type` | Stable name, e.g. `sandbox.failed` |
| `ts`, `producer`, `schema_version` | Provenance |
| `session_id` · `sandbox_id` · `task_id` · `workflow_id` · `turn_id` | Scope (nullable as appropriate) |
| `correlation_id` · `causation_id` | Causal chain |
| `tenant_id?` | Multi-tenant later |

**Serialization:** JSON (UTF-8) via the [`agent-fleet-contracts`](../../crates/agent-fleet-contracts/) crate for all commands/events by default — one object
with envelope fields + `payload`. Details and non-goals (protobuf, base64
blobs, …): [events.md §0.1](./events.md#01-serialization-recommended).

---

## Lifecycle map

Typical session, left → right. Events are listed under the phase where they
**first appear**. Commands are marked **(cmd)**.

```
 ingest → accept session → create sandbox → provision workspace
       → agent turns (loop) → finish / fail → stop or archive sandbox
```

| Phase | Typical messages |
| --- | --- |
| **1. Ingest** | `integration.task_ingested`, `integration.review_feedback_received`, `integration.human_message`, `integration.cancel_requested` |
| **2. Session accept** | `session.created`, `session.state_changed` |
| **3. Sandbox bring-up** | `sandbox.create` **(cmd)** → `sandbox.creating` → `sandbox.ready` \| `sandbox.failed` |
| **4. Workspace provision** | `driver.provision_workspace` **(cmd)** (or implicit on boot) → `workspace.provisioning` (clone, **project deps**, harness config, …) → `workspace.ready` \| `workspace.failed`. Project env is prepared on the **workspace** side of the sandbox (**per-task volume** / second container), not by mutating stable harness images. Sandbox: `crates/fleet-sandbox-driver` + named tools (`fleet-wsexec`) — see [`sandbox-workspace.md`](./sandbox-workspace.md). |
| **5. Agent turn** | `driver.start_agent_turn` **(cmd)** → `agent.turn_started` → `agent.event`* → (`agent.tool_call_proposed` → permission / proxy) → `agent.turn_completed` \| `agent.turn_failed` |
| **6. Interactive / HITL** | `integration.permission_required` → `integration.permission_decision` → `driver.permission_decision` **(cmd)**; `integration.agent_message`; `driver.inject_human_message` **(cmd)** |
| **7. Side effects out** | `integration.artifact_published` (PR, logs, screenshots), `integration.session_completed` / `session_failed` |
| **8. Teardown** | `sandbox.stop` / `archive` / `destroy` **(cmd)** → `sandbox.stopping` → `sandbox.destroyed` \| `sandbox.archived`; `driver.shutdown` **(cmd)** |
| **Anytime (async)** | `sandbox.unhealthy`, `sandbox.failed`, `driver.heartbeat`, `policy.denied` |

`agent.event` is the high-volume trace spine (thoughts, message deltas, status).

---

## By domain — outline

### A. Integration (external world ↔ control plane)

| Message | Dir | Outline |
| --- | --- | --- |
| `integration.task_ingested` | in | New work unit: source system, external ref, title/body, optional repo |
| `integration.review_feedback_received` | in | CI fail / review comment / merge conflict on an existing task or PR |
| `integration.human_message` | in | Slack/UI text into a live session |
| `integration.cancel_requested` | in | Human/system wants the session stopped |
| `integration.permission_decision` | in | allow / deny / allow-once for a prior tool call |
| `integration.agent_message` | out | Stream agent text to Slack/UI (`is_final` for last chunk) |
| `integration.permission_required` | out | Ask human/policy to approve a tool call (risk + summary) |
| `integration.session_completed` / `session_failed` | out | Terminal notify + summary + artifact links |
| `integration.artifact_published` | out | PR opened, screenshot, report URL, … |

**Producers:** integration adapters, UI gateway.  
**Consumers:** Orchestrator (inbound); adapters (outbound).

### B. Session (Orchestrator-owned facts)

| Message | Outline |
| --- | --- |
| `session.created` | Durable session row allocated; links `task_id` |
| `session.state_changed` | `Pending → AwaitingSandbox → Provisioning → Ready → Running → WaitingHITL → … → Archived` |
| `session.completed` / `failed` / `cancelled` | Terminal states (also mirrored to integrations) |
| `policy.denied` | Orchestrator refused an action without escalating to HITL |

**States:** `Pending` · `AwaitingSandbox` · `Provisioning` · `Ready` ·
`Running` · `WaitingHITL` · `Stopping` · `Completed` · `Failed` ·
`Cancelled` · `Archived`.

### C. Sandbox (compute lifecycle)

| Message | Kind | Outline |
| --- | --- | --- |
| `sandbox.create` | cmd | backend, image, resources, network policy, Driver config, workspace spec ref |
| `sandbox.stop` | cmd | graceful stop; volumes may remain |
| `sandbox.archive` | cmd | stop + keep snapshot/volumes for audit/resume |
| `sandbox.destroy` | cmd | hard delete compute + ephemeral disks |
| `sandbox.exec` | cmd | ops/debug one-shot (optional) |
| `sandbox.creating` | event | create accepted, backend call in flight |
| `sandbox.ready` | event | Driver reachable: `control_endpoint`, backend ref, actual resources |
| `sandbox.unhealthy` | event | recoverable degradation (probe fail, disk pressure) |
| `sandbox.failed` | event | **terminal** failure: `oom_killed`, `node_lost`, `image_pull`, … |
| `sandbox.stopping` | event | teardown started |
| `sandbox.destroyed` / `archived` | event | terminal teardown outcomes (`archive_ref` if retained) |

`sandbox.failed` / `unhealthy` are the reason the bus is not optional: the
hosting backend can die with no RPC caller in flight.

### D. Workspace & Driver

| Message | Kind | Outline |
| --- | --- | --- |
| `driver.provision_workspace` | cmd | repo, agent package/skills/plugins, caches, public env only |
| `driver.start_agent_turn` | cmd | prompt, mode (autonomous\|interactive), model/budget, tool policy |
| `driver.cancel_agent_turn` | cmd | abort in-flight turn |
| `driver.permission_decision` | cmd | allow/deny (+ optional precomputed tool result from MCP proxy) |
| `driver.inject_human_message` | cmd | push Slack/UI text into the agent session |
| `driver.shutdown` | cmd | graceful/immediate exit |
| `driver.heartbeat` | event | liveness + resource stats |
| `workspace.provisioning` | event | milestones (clone started, agent installed, …) |
| `workspace.ready` / `failed` | event | ready for turns, or provision error |
| `driver.shutdown_completed` | event | Driver exiting |

### E. Agent (ACP-facing, via Driver)

| Message | Kind | Outline |
| --- | --- | --- |
| `agent.turn_started` | event | ACP session active for `turn_id` |
| `agent.event` | stream | monotonic `seq` per session; thoughts, message deltas, status; redaction flag |
| `agent.tool_call_proposed` | event | tool name, args, side-effect class, `requires_secret` |
| `agent.permission_required` | event | Driver cannot auto-approve; needs Orchestrator/HITL |
| `agent.tool_call_finished` | event | local or proxied tool result summary |
| `agent.turn_completed` / `turn_failed` | event | turn terminal |

Tool calls that need secrets never execute inside the sandbox: Driver emits
`tool_call_proposed` → Orchestrator runs MCP/API proxy → returns result via
`permission_decision` / tool result path.

---

## MVP vs later

**Ship first (non-negotiable):**

Commands: `sandbox.create`, `sandbox.stop` (or `archive`),  
`driver.start_agent_turn`, `driver.cancel_agent_turn`, `driver.permission_decision`.

Events: `integration.task_ingested`, `sandbox.ready`, `sandbox.failed`,  
`workspace.ready`/`failed`, `agent.tool_call_proposed`/`permission_required`,  
`agent.event`, `agent.turn_completed`/`failed`, `session.state_changed`,  
`integration.agent_message` (if any interactive surface).

**Add when needed:** `sandbox.unhealthy` vs hard fail, archive vs destroy,
review-feedback wakeups, multi-session workflows, `sandbox.exec`, fine-grained
provisioning milestones, cost/budget events.

---

## Volume & QoS hints

| Class | Examples | Expected volume | Delivery bias |
| --- | --- | --- | --- |
| **Control** | sandbox lifecycle, session state, turn start/end | low | reliable, ordered per `sandbox_id` / `session_id` |
| **Decision** | tool_call_proposed, permission_*, human_message | low–med | reliable, low latency |
| **Trace stream** | `agent.event` (and message deltas) | high | reliable preferred; droppable only if reconstructed from durable store |
| **Heartbeat** | `driver.heartbeat` | periodic | best-effort / replaceable (latest wins) |
| **Notify** | session_completed → Slack | low | reliable, at-least-once OK |

Ordering keys and delivery expectations: see
[events.md §6](./events.md#6-ordering--delivery-expectations).

---

## Causal chain (happy path sketch)

```
integration.task_ingested
 └─ session.created
 └─ sandbox.create
     └─ sandbox.creating → sandbox.ready
         └─ workspace.provisioning → workspace.ready
             └─ driver.start_agent_turn
                 └─ agent.turn_started
                 └─ agent.event × N
                 └─ agent.tool_call_proposed
                     └─ integration.permission_required   (if HITL)
                     └─ integration.permission_decision
                     └─ driver.permission_decision
                     └─ agent.tool_call_finished
                 └─ agent.turn_completed
 └─ sandbox.archive → sandbox.archived
 └─ session.state_changed(Archived)
 └─ integration.session_completed
```

Async failure can cut in after `sandbox.creating`:

```
sandbox.failed(oom_killed)
 └─ session.state_changed(Failed)  —or—  retry with new sandbox.create
```

---

## Related

- Field-level catalogue: [`events.md`](./events.md)
- Architecture & sequences: [`README.md`](./README.md)
- Transport candidate (Zenoh): [`transport-zenoh.md`](./transport-zenoh.md)
