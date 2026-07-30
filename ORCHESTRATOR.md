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
migrations/   # 001_init.sql
tests/        # store_integration (needs DATABASE_URL)
schemas/workplan.v1.json
```

## TDD

Domain / validate / config / sandbox were developed with unit tests in-module.

```bash
cargo test --lib          # unit tests (no Postgres required)
docker compose up -d postgres
export DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable
cargo test --test store_integration
```

Host port **5433** maps to container 5432 (avoids clashing with a local Postgres on 5432).

## Local run

```bash
cp .env.example .env   # edit as needed
docker compose up -d postgres
set -a && source .env && set +a
cargo run
```

Reconcile tick lands in PR5; HTTP API in PR6.
