# Fleet Orchestrator (Rust)

Project-scoped **orchestration library** for Agent Fleet. Served by `fleet-server`
(`HOST`/`PORT`, usually `:8080`). There is no standalone binary.

Design notes (historical): [`docs/references/old-product-visions/artifacts/orchestration-service-design.md`](../../docs/references/old-product-visions/artifacts/orchestration-service-design.md)

## Technology

| Layer | Choice | Why |
| --- | --- | --- |
| Language | **Rust** (workspace edition 2024, rust-version 1.88) | Safety, static binary, async control plane |
| Async | **Tokio** | Tick loop + IO |
| Database | **PostgreSQL 16** | Source of truth |
| SQL | **SQLx** | Async Postgres |
| Migrations | moving into [`fleet-db/migrations`](../fleet-db/migrations/) | One `_sqlx_migrations` owner |
| IDs | **UUIDv5** stable board tasks | Idempotent rematerialize |
| Guest | paired `fleet-host` starts `fleet-driver` | Process or container isolation on the runner |
| Auth | Cookie BFF `/api/workspaces/{id}/fleet/*`; `/v1` is worker + `ADMIN_TOKEN` | Product UI never calls `/v1` for humans |
| Leadership | **Single process per DB** | Do not scale replicas |

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
cargo run -p fleet-server   # binds HOST/PORT (default :8080)
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
export HOST=127.0.0.1
export PORT=8080
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
