# Transport candidate: Eclipse Zenoh

Evaluation of [Zenoh](https://zenoh.io/) as the bus for Agent Fleet control-plane
commands and events (see [`events-overview.md`](./events-overview.md) and
[`events.md`](./events.md)).

Status: **investigation / draft recommendation** — not a locked decision.

---

## 1. What Zenoh is

Zenoh (* /zeno/* — “Zero Endpoint Network Overhead Handover”) is a
**pub/sub + query + compute** protocol that unifies:

| Concern | Zenoh abstraction |
| --- | --- |
| Data in motion | **Publisher / Subscriber** on hierarchical **key expressions** |
| Data at rest | **Storage** (subscriber + queryable) with pluggable volumes |
| Computations / RPC-ish | **Queryable** (+ **Querier**) — `get` triggers a computation |
| Presence | **Liveliness** tokens |
| Stronger delivery | **AdvancedPublisher / AdvancedSubscriber** (`zenoh-ext`): sequence, history cache, miss detection, recovery |

Keys are hierarchical with `/` as separator (by design — wildcards and routing
optimizations assume it). Wildcards: `*` (one chunk), `**` (any suffix/prefix).
Selectors add `?params` for queries. Values are bytes + an **encoding**
(JSON, plain text, custom, …).

Topology is flexible at runtime: **peer-to-peer, client/router (brokered), or
hybrid**. Reference router: `zenohd`. First-class **Rust** implementation
(Eclipse Zenoh); also C/C++/Python/… — fits a Rust control plane. For origin
and positioning vs MQTT/DDS, see the
[official overview](https://zenoh.io/docs/overview/what-is-zenoh/).

Wire overhead is intentionally tiny; the pitch is high throughput / low latency
from edge to datacenter. In practice for us: low-latency session streaming
(Driver ↔ Orchestrator) and fan-out of control events matter more than
microcontroller support.

---

## 2. Why it maps well to our architecture

Our control plane needs **more than a dumb message queue**:

| Need | Kafka/NATS-style | Zenoh fit |
| --- | --- | --- |
| Async facts (sandbox OOM, turn completed) | topics / subjects | **pub/sub** on key expressions |
| Commands with a reply (create sandbox ack, tool result) | request-reply or separate reply topic | **queryable** (`get` → computation) *or* command put + event put |
| “Who is alive?” (Driver, Sandbox Manager shard) | heartbeats on a topic | **liveliness** tokens + heartbeats |
| Late joiner / UI reconnect sees recent state | consumer groups + log retention, or DB | **Advanced pub/sub** history + optional **storage** |
| Subscribe to one session’s trace stream | topic-per-session or filter | key `…/session/{id}/agent/**` + `*`/`**` |
| Subscribe to all sandbox failures fleet-wide | topic + filter | `fleet/*/sandbox/*/event/failed` |
| Durable audit log | log retention / connect sink | **Postgres remains SoT**; Zenoh storage optional cache |
| MCP-style proxied call | out of band gRPC | queryable on Orchestrator, or command/event pair |
| Multi-tenant key isolation | ACL + topic prefix | session **namespace** prefix (Zenoh 1.x) |

We already said the *contract* is event-shaped and transport is an
implementation choice ([architecture §4](./README.md#4-event-driven-control-plane)).
Zenoh is attractive because **one substrate** covers pub/sub, request/reply-ish
queryables, presence, and optional geo/storage — instead of Kafka + a separate
RPC channel + a presence hack.

---

## 3. Mapping: message kinds → Zenoh primitives

| Message class | Examples | Recommended Zenoh use |
| --- | --- | --- |
| **Lifecycle events** | `sandbox.ready`, `sandbox.failed`, `session.state_changed` | `put` on event keys; **AdvancedPublisher** if consumers must not miss |
| **High-volume streams** | `agent.event`, `integration.agent_message` chunks | `put` on stream keys; reliable + sequenced; consider SHM for co-located |
| **Heartbeats** | `driver.heartbeat` | `put` with latest-wins / small cache **or** liveliness + light stats key |
| **Presence** | Driver up, SM replica up | **liveliness** token per instance |
| **Commands (fire + later event)** | `sandbox.create`, `sandbox.stop` | `put` on command key **or** queryable on Sandbox Manager; outcome still a lifecycle event |
| **Commands needing sync reply** | `driver.permission_decision` with tool result body; small RPCs | **queryable** on Driver/Orchestrator (`get` with payload) |
| **HITL / integration ingress** | `task_ingested`, `human_message` | `put` on integration keys; Orchestrator subscribes |
| **Durable “what is the session state?”** | UI refresh | Postgres primary; optional Zenoh **storage** or queryable on Orchestrator for hot state |
| **Trace durability** | audit / replay | append to **Postgres** (and object store for blobs); Zenoh is the live path |

Rule of thumb we should keep:

- **Zenoh = live coordination plane** (fast, interest-scoped, presence-aware).
- **Postgres = system of record** (sessions, event log, traces, artifacts metadata).

Do not treat Zenoh storage plugins as a replacement for the audit database
unless we deliberately design for that later.

---

## 4. Proposed key-space

Zenoh wants a **strict hierarchy** and `/` separators. Prefer
`entity/id/aspect` over stuffing multiple discriminants into one chunk
(so `*` wildcards stay efficient).

```
fleet/{tenant}/…
```

### 4.1 Events (facts)

```
fleet/{tenant}/session/{session_id}/event/{type}
fleet/{tenant}/session/{session_id}/agent/{turn_id}/event/{type}
fleet/{tenant}/session/{session_id}/agent/{turn_id}/stream/{seq}     # optional fine key for agent.event
fleet/{tenant}/sandbox/{sandbox_id}/event/{type}
fleet/{tenant}/task/{task_id}/event/{type}
fleet/{tenant}/integration/{name}/event/{type}
```

Examples:

```
fleet/default/sandbox/9f3a…/event/ready
fleet/default/sandbox/9f3a…/event/failed
fleet/default/session/1c2b…/event/state_changed
fleet/default/session/1c2b…/agent/7e0d…/stream/0042
fleet/default/integration/slack/event/human_message
```

Wildcard subscriptions:

| Interest | Key expression |
| --- | --- |
| All sandbox failures | `fleet/*/sandbox/*/event/failed` |
| Everything for one session | `fleet/{t}/session/{sid}/**` |
| All agent streams fleet-wide (ops) | `fleet/*/session/*/agent/**` |
| One integration inbound | `fleet/{t}/integration/slack/event/*` |

Payload: **JSON** body = our [common envelope](./events.md#0-common-envelope)
+ `payload` domain fields ([serialization](./events.md#01-serialization-recommended)).
Zenoh encoding: `application/json` / `AppJson`. CBOR only as a later,
opt-in optimization for hot streams — not the default.

### 4.2 Commands

Two viable patterns (pick one per interface; document it):

**A. Command topics (symmetric with events)**

```
fleet/{tenant}/sandbox/{sandbox_id}/cmd/{type}      # create may use sandbox_id pre-assigned
fleet/{tenant}/sandbox/cmd/create                   # if id assigned by SM
fleet/{tenant}/session/{session_id}/cmd/{type}      # start_turn, cancel, inject_human, …
fleet/{tenant}/driver/{sandbox_id}/cmd/{type}       # addressed to Driver in that sandbox
```

Sandbox Manager subscribes to `fleet/{t}/sandbox/**/cmd/**` (or a dedicated
queue key). Driver subscribes to `fleet/{t}/driver/{sandbox_id}/cmd/**`.

**B. Queryables (RPC)**

```
fleet/{tenant}/sandbox-manager/q/create
fleet/{tenant}/driver/{sandbox_id}/q/start_turn
fleet/{tenant}/driver/{sandbox_id}/q/permission_decision
fleet/{tenant}/orchestrator/q/session/{session_id}/state
```

Orchestrator `get`s the queryable with the command payload; handler returns
ack `{ accepted, message_id }` quickly. **Long outcomes still published as
events** (`sandbox.ready`, `agent.turn_completed`, …).

**Recommendation for MVP:**  
- **Events → pub/sub** (Advanced where miss is painful).  
- **Hot path Driver commands** (permission decision, cancel) → **queryable** for
  low latency + explicit reply.  
- **Sandbox create/stop** → pub/sub command *or* queryable; SM is one logical
  service so queryable is natural.

### 4.3 Liveliness

```
fleet/{tenant}/liveliness/orchestrator/{instance_id}
fleet/{tenant}/liveliness/sandbox-manager/{instance_id}
fleet/{tenant}/liveliness/driver/{sandbox_id}
fleet/{tenant}/liveliness/integration/{name}/{instance_id}
```

Orchestrator watches `fleet/{t}/liveliness/driver/**` to detect Driver death
even before `sandbox.failed` arrives (or to correlate).

### 4.4 Optional admin / metrics

```
fleet/{tenant}/metrics/sandbox/{sandbox_id}
fleet/{tenant}/admin/…
```

Keep out of the critical event path.

---

## 5. Reliability & ordering strategy

| Stream | Need | Zenoh approach |
| --- | --- | --- |
| Sandbox lifecycle | no lost `failed`/`ready` | AdvancedPublisher + AdvancedSubscriber; consumer idempotent on `message_id` |
| Session state | no lost transitions | same; state machine ignores stale transitions |
| Agent trace | ordered per session | `seq` in payload; single publisher (Driver); Advanced sequencing |
| Heartbeats | latest wins | plain put, small history or overwrite storage key |
| Permission path | low latency, exactly-once *effect* | queryable + idempotent `tool_call_id` handling |

**At-least-once + idempotent consumers** remains the app-level contract
([events.md §6](./events.md#6-ordering--delivery-expectations)). Zenoh Advanced
pub/sub reduces miss windows; it does not remove the need for
`message_id` / state-machine dedupe.

Postgres outbox pattern (optional hardening):

1. Orchestrator writes event to Postgres in the same transaction as state change.
2. A publisher process `put`s to Zenoh (or SM/Driver publish directly and
   Orchestrator persists on consume — choose one authority).

For MVP, **Driver/SM publish live events; Orchestrator persists on ingest** is
simpler. Move to outbox if we see loss under crash.

---

## 6. Deployment sketch

```
                 ┌──────────── zenohd (router) ────────────┐
                 │  (or mesh of routers / peer mode)       │
                 └───────┬──────────┬──────────┬───────────┘
                         │          │          │
              Orchestrator    Sandbox Mgr   Integration
              (+ queryables)  (+ queryables)  adapters
                         │
                    ┌────┴────┐
                    │ network │  (restricted path into sandbox)
                    └────┬────┘
                      Driver  (client mode to router)
                         │ ACP
                      Coding agent
```

- **Control plane services**: client or peer to `zenohd` on the trusted network.
- **Driver inside sandbox**: **client mode** to a router endpoint exposed only
  on the control channel (not general internet). Sandbox egress policy must
  allow this path and little else.
- **Self-hosted workers** (customer network): Zenoh peer/bridge or a
  site-local `zenohd` that links home — location transparency is part of the
  Zenoh pitch; still need authn/authz on keys.
- **Auth**: mTLS (or Zenoh access control config) between Driver and router;
  never put long-lived cloud secrets in the sandbox — only a short-lived
  session token scoped to `fleet/{t}/driver/{sandbox_id}/**` and matching
  session keys.

---

## 7. Comparison (honest)

| | Zenoh | NATS | Kafka / Redpanda | gRPC only |
| --- | --- | --- | --- | --- |
| Pub/sub | excellent, hierarchical keys | excellent | excellent (log) | poor (need streams) |
| Request/reply | queryable first-class | first-class | awkward | native |
| Durable log / replay | via storage plugin or app DB | JetStream | native strength | no |
| Ops familiarity | younger; robotics/edge heavy | common in cloud | very common | universal |
| Rust ergonomics | native | good | good (rdkafka etc.) | tonic excellent |
| Multi-consumer audit | yes + wildcards | yes | yes | no |
| Footprint / latency | designed low | low | heavier | low |
| “Cool kids” factor | high in robotics/edge | high in cloud-native | enterprise default | — |

**When Zenoh is a strong fit for us**

- One fabric for Orchestrator ↔ SM ↔ many Drivers (dynamic endpoints).
- Interest-based routing: UI watches one session; ops watches all failures.
- Liveliness for disposable Drivers.
- Queryables for permission/tool proxy round-trips without a second RPC stack.
- Future self-hosted / edge workers without redesigning the bus.

**When to hesitate**

- Team wants boring, hireable ops (Kafka/NATS win on familiarity).
- Primary need is a weeks-long immutable event log with consumer groups —
  Kafka-shaped; we’d still keep Postgres, but Kafka integrates that mental
  model out of the box.
- Need battle-tested multi-tenant SaaS ACLs and cloud marketplace mental model
  tomorrow morning.
- Advanced pub/sub / some APIs have moved quickly (stabilize versions; pin
  crates; read changelogs — e.g. Firesong advanced pub/sub, later heartbeat
  recovery improvements).

**Pragmatic recommendation**

1. **Adopt Zenoh as the live control/data plane** for session + sandbox +
   driver messaging in the prototype / MVP.
2. **Keep Postgres as durable SoT** for sessions, traces, and audit.
3. **Do not couple** domain event *names* or payloads to Zenoh — only the
   key-space and delivery adapter. Swapping to NATS later should be an
   adapter change, not a redesign of [`events.md`](./events.md).
4. Spike early: Driver in a pod, client-mode to `zenohd`, AdvancedSubscriber
   on Orchestrator for `sandbox/**/event/**` and queryable permission path;
   kill the pod and confirm `sandbox.failed` + liveliness loss both surface.

---

## 8. Spike checklist

- [ ] Run `zenohd` locally; Orchestrator + fake SM + fake Driver as Rust clients
- [ ] Publish `sandbox.ready` / `sandbox.failed`; subscribe with wildcards
- [ ] AdvancedPublisher on Driver `agent.event` stream; kill publisher mid-stream;
      confirm miss detection / recovery behaviour we rely on
- [ ] Queryable `permission_decision` RTT under load
- [ ] Liveliness drop when Driver process exits
- [ ] Network policy: sandbox can reach only Zenoh control endpoint
- [ ] Payload size: large trace frames / tool args — fragmentation OK?
- [ ] Authn: token scoped to sandbox key prefix
- [ ] Compare one alternative (NATS) on the same spike scripts if decision is contested

---

## 9. References

- [Zenoh home](https://zenoh.io/)
- [What is Zenoh?](https://zenoh.io/docs/overview/what-is-zenoh/)
- [Abstractions (keys, pub/sub, queryable, storage)](https://zenoh.io/docs/manual/abstractions/)
- [Eclipse Zenoh GitHub](https://github.com/eclipse-zenoh/zenoh)
- [Advanced pub/sub (Firesong 1.1)](https://zenoh.io/blog/2024-12-12-zenoh-firesong-1.1.0/)
- [Gozuryū notes on advanced pub/sub recovery](https://zenoh.io/blog/2025-04-14-zenoh-gozuryu/)
- Our event contract: [`events-overview.md`](./events-overview.md), [`events.md`](./events.md)
