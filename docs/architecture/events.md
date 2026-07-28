# Event & command catalogue

Contract for the event-driven control plane described in
[`README.md`](./README.md). For a shorter lifecycle-oriented outline, start
with [`events-overview.md`](./events-overview.md). Transport is orthogonal to
this schema; Zenoh is evaluated in [`transport-zenoh.md`](./transport-zenoh.md).

Status: **draft** — field lists are the minimum needed to orchestrate
sandbox + agent sessions. Extend, do not silently rename.

---

## 0. Common envelope

Every command and event:

| Field | Type | Notes |
| --- | --- | --- |
| `message_id` | UUID | Unique per message (idempotency key for consumers) |
| `type` | string | e.g. `sandbox.ready`, `agent.tool_call_proposed` |
| `ts` | RFC3339 | Producer time |
| `producer` | string | `orchestrator`, `sandbox-manager`, `driver`, `integration:<name>` |
| `schema_version` | semver | Payload schema |
| `session_id` | UUID? | Set when scoped to a session |
| `sandbox_id` | UUID? | Set when scoped to a sandbox |
| `task_id` | UUID? | Upstream work item (Jira issue, GH issue, …) |
| `workflow_id` | UUID? | Multi-session workflow, if any |
| `turn_id` | UUID? | Agent turn within a session |
| `correlation_id` | UUID | Ties a whole causal chain (usually = originating task/session) |
| `causation_id` | UUID? | `message_id` of the direct cause |
| `tenant_id` | string? | Multi-tenant deployments |

Commands additionally carry `reply_to` / expected ack semantics if the
transport supports request-reply; **success/failure of long-running work
is still expressed as a later event**, not only as an RPC status.

---

## 0.1 Serialization (recommended)

**Default for all commands and events: JSON (UTF-8), one object per message.**

| Concern | Choice |
| --- | --- |
| Wire format | JSON object |
| Charset | UTF-8 |
| Zenoh encoding | `application/json` (Zenoh `AppJson` / equivalent) |
| Schema evolution | `schema_version` (semver) on every message + additive fields |
| IDs | UUID **strings** (`"1c2b…"`), not binary |
| Timestamps | RFC3339 strings with timezone (`"2026-07-28T12:34:56.789Z"`) |
| Enums / `type` | stable lowercase strings (`"sandbox.failed"`, `"oom_killed"`) |
| Null vs omit | omit unknown/optional fields; use `null` only when “explicitly cleared” matters |
| Binary / bulky data | **not** in the JSON body — store in object storage / Postgres bytea and put a URI or id in the event |
| Rust | crate [`agent-fleet-contracts`](../../crates/agent-fleet-contracts/) (`serde` + `serde_json`) |

### Why JSON (for now)

- Debuggable end-to-end: `zenoh` CLI, logs, Slack-adjacent tooling, browser UI.
- Integrations already speak JSON (Jira/GitHub/Slack webhooks).
- Fast enough for control events and moderate `agent.event` rates; we are not
  in a hard real-time robotics path on the control plane.
- Avoids locking a protobuf/FlatBuffers schema before the event catalogue
  stabilizes.
- Transport-agnostic: same bytes on Zenoh, NATS, or a Postgres `JSONB` outbox.

### What is *not* recommended as the default

| Format | When it might appear | Why not default |
| --- | --- | --- |
| Protobuf / gRPC messages | Optional *intra*-process or a dedicated ACP bridge | Poor log greppability; schema churn early; second toolchain |
| MessagePack / CBOR | Optional later for **high-volume** `agent.event` streams if profiling demands it | Extra ops/debug cost; dual codecs |
| Raw text / ad-hoc lines | Never for control plane | No envelope, no versioning |
| Embedding large blobs (base64 screenshots, full build logs) | Never in the bus message | Blows up memory and bus; use artifact refs |

### Message shape

Every bus message is a single JSON object:

```json
{
  "message_id": "018f…",
  "type": "sandbox.failed",
  "ts": "2026-07-28T12:34:56.789Z",
  "producer": "sandbox-manager",
  "schema_version": "0.1.0",
  "session_id": "1c2b…",
  "sandbox_id": "9f3a…",
  "task_id": null,
  "workflow_id": null,
  "turn_id": null,
  "correlation_id": "1c2b…",
  "causation_id": "018e…",
  "tenant_id": "default",
  "payload": {
    "severity": "failed",
    "reason_code": "oom_killed",
    "message": "Pod OOMKilled by kubelet",
    "restart_count": 1
  }
}
```

Conventions:

1. **Envelope fields at the top level** (table above); domain fields under
   `payload` (or flattened only if we later publish a frozen schema — prefer
   `payload` so envelope parsing stays uniform).
2. **`type` is the routing/dispatch key** in application code; Zenoh key
   expressions *also* carry domain/id/type for interest filtering — they must
   agree with `type` / ids in the body.
3. **`schema_version` is for the payload shape** of that `type` (or a global
   envelope version if we keep them in lockstep). Consumers ignore unknown
   fields; reject only hard incompatibilities (major bump).
4. **Stream chunks** (`agent.event`, `integration.agent_message`) use the same
   envelope; put deltas in `payload` (`{"seq": 42, "kind": "message_delta", "text": "…"}`).
5. **Queryable replies** (Zenoh `get` responses) use the same JSON object shape
   for acks: e.g. `{"message_id":"…","type":"cmd.ack","payload":{"accepted":true}}`.

### Evolution path

```
now     JSON everywhere (commands, events, acks, traces on the bus)
later   if agent.event volume hurts: CBOR or length-prefixed JSON lines
        for that stream only, negotiated via encoding + schema_version
never   silently mix formats on one key without encoding metadata
```

Postgres: store the same JSON (JSONB) for the durable event log so bus and DB
do not diverge.

---

## 1. Integration domain

### 1.1 Inbound (Integration → Orchestrator)

#### `integration.task_ingested`
A new unit of work arrived (Jira ticket, GitHub issue, manual UI submit).

```
task_id, source (jira|github|gitlab|linear|ui|…),
external_ref (URL / key),
title, body, labels[],
repo_url?, default_branch?,
priority?, requester?,
attachments[]?
```

#### `integration.review_feedback_received`
CI failure or PR review comment that should wake an existing session / spawn a follow-up turn.

```
task_id, session_id?,
pr_url, comment_id?,
kind (ci_failure|review_comment|merge_conflict),
body, author?
```

#### `integration.human_message`
Interactive HITL input (Slack thread reply, UI chat, approval form free text).

```
session_id, turn_id?,
channel (slack|teams|ui|…),
thread_ref,
text, author,
attachments[]?
```

#### `integration.cancel_requested`
Human or external system asks to stop the session.

```
session_id / task_id,
reason?, requester?
```

#### `integration.permission_decision`
Human or policy service answered a prior `permission_required`.

```
session_id, turn_id,
tool_call_id,
decision (allow|deny|allow_once|always_allow_for_session),
decided_by, comment?
```

### 1.2 Outbound (Orchestrator → Integration)

#### `integration.agent_message`
Stream model / agent text toward an interactive surface.

```
session_id, turn_id,
channel, thread_ref,
text, is_final (bool),
markdown?
```

#### `integration.permission_required`
Ask a human (or external policy) to approve a tool call.

```
session_id, turn_id, tool_call_id,
tool_name, args_summary, risk_level,
channel, thread_ref?
```

#### `integration.session_completed` / `integration.session_failed`
Terminal notifications for the task owners.

```
session_id, task_id,
status, summary,
artifacts[] (pr_url, log_url, …),
error?
```

#### `integration.artifact_published`
PR opened, screenshot uploaded, report written, etc.

```
session_id, task_id,
kind (pull_request|file|url|screenshot|log),
uri, title?, metadata?
```

---

## 2. Sandbox domain

### 2.1 Commands (Orchestrator → Sandbox Manager)

#### `sandbox.create`
Allocate compute and start the Driver entrypoint.

```
sandbox_id (pre-assigned by Orchestrator or Manager),
session_ids[] (initial occupants),
backend (k8s|vm|podman|self_hosted|…),
image / template,
resources { cpu, memory, disk, gpu? },
network_policy { egress_mode, allowlist[] },
driver {
  image_or_binary,
  config_blob_ref,          # how Driver finds Orchestrator endpoint, session tokens
  workspace_spec_ref        # repo, caches, agent package — see provision
},
labels{},
ttl? / idle_timeout?
```

#### `sandbox.stop`
Graceful stop: signal Driver, then stop compute. Volumes may remain.

```
sandbox_id, reason, grace_period?
```

#### `sandbox.archive`
Stop compute and retain workspace snapshot / volumes for later resume or audit.

```
sandbox_id, retain { volumes, snapshot, logs }, reason?
```

#### `sandbox.destroy`
Hard delete compute + ephemeral storage. Irreversible.

```
sandbox_id, reason?
```

#### `sandbox.exec` (optional, debug/ops)
Run a one-shot command in the sandbox network namespace (not the agent).

```
sandbox_id, command[], timeout?
```

### 2.2 Events (Sandbox Manager → bus)

| Type | When |
| --- | --- |
| `sandbox.creating` | Accept create; backend call in flight |
| `sandbox.ready` | Compute up, Driver process started, control endpoint reachable |
| `sandbox.unhealthy` | Probe failed / disk pressure / partial degradation; may recover |
| `sandbox.failed` | Terminal failure: OOMKilled, VM died, image pull error, admission reject |
| `sandbox.stopping` | Stop/archive/destroy accepted |
| `sandbox.destroyed` | Compute gone, ephemeral disks gone |
| `sandbox.archived` | Compute gone, retained artifacts addressable via `archive_ref` |

#### `sandbox.ready` payload

```
sandbox_id,
backend, backend_ref (pod name / vm id),
control_endpoint (Driver dial address),
network { ip?, hostname? },
resources_actual {},
started_at
```

#### `sandbox.failed` / `sandbox.unhealthy` payload

```
sandbox_id,
severity (unhealthy|failed),
reason_code (oom_killed|node_lost|image_pull|admission|probe_timeout|unknown),
message,
restart_count?,
backend_raw?   # opaque backend status for ops
```

Failure events **must** be emitted even when no `CreateSandbox` caller is
waiting — this is the core argument for an event bus.

---

## 3. Workspace / Driver domain

Workspace provisioning is the Driver's job *after* `sandbox.ready`.

### 3.1 Commands (Orchestrator → Driver)

#### `driver.provision_workspace`
May be implicit on Driver boot via config; explicit command allows re-provision.

```
session_id, sandbox_id,
repo { url, ref, depth?, auth_via (proxy|deploy_key_ref) },
agent { package, version, skills[], plugins[], tools[] },
caches[] { name, uri, mount_path },
env_public{} ,            # non-secret env only
network_profile,
success_criteria?
```

#### `driver.start_agent_turn`

```
session_id, turn_id,
prompt (text + optional structured parts),
mode (autonomous|interactive),
model?, budget?,
tool_policy_ref?,
parent_turn_id?
```

#### `driver.cancel_agent_turn`

```
session_id, turn_id, reason?
```

#### `driver.permission_decision`

```
session_id, turn_id, tool_call_id,
decision (allow|deny|…),
result_override?          # if Orchestrator already executed a proxied tool
```

#### `driver.inject_human_message`

```
session_id, turn_id?,
text, author, channel?
```

#### `driver.shutdown`

```
session_id?, reason, mode (graceful|immediate)
```

### 3.2 Events (Driver → bus / Orchestrator)

| Type | When |
| --- | --- |
| `driver.heartbeat` | Liveness + resource stats |
| `workspace.provisioning` | Progress milestones (clone started, agent installed, …) |
| `workspace.ready` | Agent binary present, repo on disk, ready for turns |
| `workspace.failed` | Provision error (clone denied, install failed, …) |
| `agent.turn_started` | ACP session active for `turn_id` |
| `agent.event` | Generic ACP/trace event (thought, message delta, status) |
| `agent.tool_call_proposed` | Agent wants to invoke a tool |
| `agent.permission_required` | Driver cannot auto-approve; needs Orchestrator/HITL |
| `agent.tool_call_finished` | Tool finished (local or proxied) |
| `agent.turn_completed` | Turn finished successfully |
| `agent.turn_failed` | Turn error / cancelled / agent crash |
| `driver.shutdown_completed` | Driver exiting |

#### `agent.tool_call_proposed` payload

```
session_id, turn_id, tool_call_id,
tool_name,
arguments (json),          # may be redacted in stored form
side_effect_class (read|write|network|destructive),
requires_secret (bool),
suggested_policy_action?
```

#### `agent.event` payload (trace spine)

```
session_id, turn_id,
seq (monotonic per session),
acp_method / kind,
payload (json),
redaction (none|partial|full)
```

Traces are appended to Postgres (and optionally object storage for large
blobs). The Orchestrator is the authority for durable storage even if the
Driver buffers briefly.

---

## 4. Session domain (Orchestrator-owned facts)

Emitted by the Orchestrator for UI, audit, and Integrations:

| Type | Meaning |
| --- | --- |
| `session.created` | Session record allocated |
| `session.state_changed` | `Pending → AwaitingSandbox → … → Archived` |
| `session.trace_appended` | Optional coarse signal; fine-grained is `agent.event` |
| `session.completed` | Success terminal |
| `session.failed` | Failure terminal |
| `session.cancelled` | Cancelled by user/system |
| `policy.denied` | Orchestrator refused an action without HITL |

#### Session states

`Pending` · `AwaitingSandbox` · `Provisioning` · `Ready` · `Running` ·
`WaitingHITL` · `Stopping` · `Completed` · `Failed` · `Cancelled` · `Archived`

---

## 5. Minimal event set to operate the system

If implementing MVP, these are the **non-negotiable** messages:

**Commands**

1. `sandbox.create`
2. `sandbox.stop` (and/or `sandbox.archive`)
3. `driver.start_agent_turn`
4. `driver.cancel_agent_turn`
5. `driver.permission_decision`

**Events**

1. `integration.task_ingested` (or UI equivalent)
2. `sandbox.ready`
3. `sandbox.failed`  ← async failure; do not skip
4. `workspace.ready` / `workspace.failed`
5. `agent.tool_call_proposed` / `agent.permission_required`
6. `agent.event` (trace)
7. `agent.turn_completed` / `agent.turn_failed`
8. `session.state_changed`
9. `integration.agent_message` (if any interactive surface exists)

Everything else is additive (unhealthy vs failed, archive vs destroy,
review-feedback wakeups, multi-session workflows).

---

## 6. Ordering & delivery expectations

| Stream | Ordering key | Delivery |
| --- | --- | --- |
| Sandbox lifecycle | `sandbox_id` | at-least-once; consumers idempotent on `message_id` / state machine |
| Session / agent | `session_id` (+ `seq` for `agent.event`) | at-least-once; `seq` monotonic for trace reconstruct |
| Integration inbound | `task_id` preferred | at-least-once; dedupe on external_ref |

Consumers must tolerate duplicates and out-of-order *across* domains
(e.g. a late `agent.event` after `sandbox.failed`). Within a single
`session_id` agent stream, `seq` restores order.

---

## 7. Example causal chain

```
integration.task_ingested          correlation=C
  └─ session.created               correlation=C
  └─ sandbox.create (cmd)          correlation=C
       └─ sandbox.creating
       └─ sandbox.ready            causation=create
            └─ workspace.provisioning
            └─ workspace.ready
                 └─ driver.start_agent_turn (cmd)
                      └─ agent.turn_started
                      └─ agent.event (×N)
                      └─ agent.tool_call_proposed
                           └─ integration.permission_required
                           └─ integration.permission_decision
                           └─ driver.permission_decision (cmd)
                           └─ agent.tool_call_finished
                      └─ agent.turn_completed
                 └─ session.state_changed(Ready)
  └─ sandbox.archive (cmd)
       └─ sandbox.archived
  └─ session.state_changed(Archived)
  └─ integration.session_completed
```

Async failure insertable at any time after `sandbox.creating`:

```
sandbox.failed(reason=oom_killed)
  └─ session.state_changed(Failed)  or  retry → new sandbox.create
```
