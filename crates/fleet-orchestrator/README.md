# Fleet Orchestrator (Rust)

Project-scoped **orchestration service** for Agent Fleet. Pure-logic control plane after a `WorkPlan` is accepted.

Design: [`artifacts/orchestration-service-design.md`](artifacts/orchestration-service-design.md)

## Technology (MVP)

| Layer | Choice | Why |
| --- | --- | --- |
| Language | **Rust** (1.75+, edition 2021) | Safety, static binary, async control plane |
| Async | **Tokio** | Tick loop + IO |
| Database | **PostgreSQL 16** | Source of truth, advisory locks (later), TX materialize |
| SQL | **SQLx** (runtime-checked queries) | Async Postgres without heavy ORM |
| Migrations | `sqlx::migrate!` + `migrations/` | Forward-only greenfield |
| IDs | **UUIDv5** stable board tasks | Idempotent rematerialize |
| Sandbox | `SandboxRuntime` + **`stub`** / **`process`** / **`k8s`** | In-process fake, exec + pidfile, or `FleetSandbox` CRs |
| Notify / sync | `NotificationSink` noops; `ExternalBoardSync` log wired in tick | PR8 webhooks still open; PR10 landed |
| Auth (later HTTP) | `ADMIN_TOKEN` / session worker tokens (SHA-256) | Design K24/K31 |
| Leadership | **Single process per DB** | Do not scale replicas in MVP |

**Stack note:** The design doc originally suggested Go for MVP speed; this repo implements orchestration in **Rust** as the chosen production language. Domain semantics are unchanged.

## Layout

```
src/
  domain/     # statuses, CanTransition, retry, capacity, plan types, stable IDs
  validate/   # ValidatePlan (deps, cycles, limits)
  config/     # env knobs
  sandbox/    # workspace prep + remote assign payload
  notify/     # NotificationSink + Noop/Log (PR4)
  sync/       # ExternalBoardSync + Noop/Log (PR4)
  store/      # Postgres repositories + migrate (PR3)
  orch/       # reconcile tick PR5a–c (materialize → spawn → cancel → rollup)
migrations/   # 001_init.sql
tests/        # store_integration, tick_integration (need DATABASE_URL)
schemas/workplan.v1.json
```

## TDD

Domain / validate / config / sandbox / orch pure steps were developed with unit tests in-module.

```bash
cargo test --lib          # unit tests (no Postgres required)
docker compose up -d postgres
export DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable
cargo test --test store_integration --test tick_integration
```

Host port **5433** maps to container 5432 (avoids clashing with a local Postgres on 5432).

## Local run

```bash
cp .env.example .env   # edit as needed
docker compose up -d postgres
set -a && source .env && set +a
cargo run
```

**Landed:** PR5a–c tick, PR6 HTTP API (+ OpenAPI), PR7 process sandbox, PR9 fixtures + E2E, PR10 external board sync wiring.  
**Next (optional / polish):** PR8 metrics/webhook notify.

## Fixtures & demo (PR9)

Plan JSON under `fixtures/plans/`:

| File | Scenario |
|------|----------|
| `two_task_chain.json` | Happy dependency chain |
| `single_task_retry.json` | Fail → retry budget |
| `single_task_hitl.json` | Blocked + HITL resume |
| `three_task_diamond.json` | Fan-out / join graph |

```bash
# Integration + E2E matrix (needs Postgres)
export DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable
cargo test -p fleet-orchestrator --test e2e_matrix --locked

# Scripted demo against a running orch (ADMIN_TOKEN + /v1)
cargo run -p fleet-server         # /v1 is on the unified API
ADMIN_TOKEN=dev-admin-token-change-me SCENARIO=all ./scripts/demo-orch.sh
```

## HTTP API (PR6)

Control plane HTTP is `fleet-server` (`/v1` fallback). There is no standalone `fleet-orchestrator` binary.

OpenAPI: [`api/openapi.yaml`](api/openapi.yaml) — also served at `GET /openapi.yaml`.

| Method | Path | Auth |
|--------|------|------|
| GET | `/healthz`, `/readyz`, `/openapi.yaml` | none |
| POST | `/v1/projects` | admin bearer |
| GET | `/v1/projects/{id}` | admin |
| POST | `/v1/projects/{id}/runs/fixture` | admin |
| POST | `/v1/projects/{id}/admin/tick` | admin |
| POST | `/v1/projects/{id}/runs/{run}/cancel` | admin |
| GET | `/v1/projects/{id}/tasks` | admin |
| POST | `/v1/projects/{id}/workers/{session}/heartbeat` | worker token |
| POST | `/v1/projects/{id}/workers/{session}/status` | worker token |

```bash
export DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable
export ADMIN_TOKEN=dev-admin-token-change-me
export DATA_DIR=/tmp/fleet-data
export HTTP_ADDR=127.0.0.1:8081
cargo run -p fleet-server
```

## External board sync (PR10)

When `project_configs.auto_sync_external` is true, each tick pushes tasks that have
`external_ref` (`{ "provider", "key" }`) through `ExternalBoardSync` (default:
`LogBoardSync` — logs only, no Jira client).

```sql
UPDATE project_configs SET auto_sync_external = true WHERE project_id = $1;
```

## Process guest (PR7)

Production tick assigns to a paired `fleet-host`, which starts
`fleet-driver`. The guest:

1. Authenticates with the session `FLEET_WORKER_TOKEN`
2. POSTs `running` / heartbeats / terminal `success|failed|blocked` to `/v1/.../workers/{session}/…`
3. Runs ACP (`FLEET_HARNESS=mock|grok`) and writes `artifacts/result.json` + `artifacts/transcript.md`

The plane does not run an in-process sandbox. Tests assign to a paired
host and complete work through worker status reports.

See [`crates/fleet-driver/README.md`](../fleet-driver/README.md) and
[`docs/components/worker-host/README.md`](../../docs/components/worker-host/README.md).

| Env | Role |
|-----|------|
| `FLEET_WORK_CMD` | Shell work to run (optional) |
| `FLEET_FORCE_FAIL=1` | Report `failed` (tests/fixtures) |
| `FLEET_FORCE_BLOCKED=1` | Report `blocked` for HITL |
| `FLEET_HEARTBEAT_INTERVAL_S` | Heartbeat period (default 2) |
