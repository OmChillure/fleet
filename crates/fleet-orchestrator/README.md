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
| Sandbox | `SandboxRuntime` trait + **`stub`** | Tick semantics without containers (PR4) |
| Notify / sync | `NotificationSink` / `ExternalBoardSync` noops | Wire points for PR8/PR10 |
| Auth (later HTTP) | `ADMIN_TOKEN` / session worker tokens (SHA-256) | Design K24/K31 |
| Leadership | **Single process per DB** | Do not scale replicas in MVP |

**Stack note:** The design doc originally suggested Go for MVP speed; this repo implements orchestration in **Rust** as the chosen production language. Domain semantics are unchanged.

## Layout

```
src/
  domain/     # statuses, CanTransition, retry, capacity, plan types, stable IDs
  validate/   # ValidatePlan (deps, cycles, limits)
  config/     # env knobs
  sandbox/    # SandboxRuntime + StubRuntime (PR4)
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

**PR5a–c landed:** full reconcile loop with stub workers (inbox, spawn, stuck/kill/retry, cancel drain, orphan reap, rollup).  
**Next:** PR6 HTTP API (projects, fixture plan, workers, cancel, admin/tick).

## HTTP API (PR6)

Standalone control plane (default bind from `HTTP_ADDR`, often `:8080` — avoid clashing with `fleet-server`).

| Method | Path | Auth |
|--------|------|------|
| GET | `/healthz`, `/readyz` | none |
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
cargo run -p fleet-orchestrator
```
