# Cases — durable general-purpose workflows

Cases are the **generalized platform layer**: long-lived work with backend-owned
state. Coding agent fleets (orchestrator runs) are an **optional executor**.
Integrations (email, calendar, …) plug in later as tools.

## Mental model

| Piece | Owns | Lifetime |
| --- | --- | --- |
| **Case** | `status`, free-form `stage`, `context` JSON, `next_wake_at`, event log | Days |
| **Decision** | Next action (`wait`, `set_stage`, `complete`, `use_tool`, …) | One wake |
| **Orchestrator run** | Multi-agent plan / sandboxes | Minutes–hours |
| **Integration tool** | External I/O (secrets on control plane) | Per call |

```
policy_md + observations
        │
        ▼
   wake (ticker) ──► decide ──► apply transition ──► wait / complete / fail
        ▲                              │
        └──────── next_wake_at ◄───────┘
```

The LLM (or heuristic) is **not** the runtime. Postgres is.

## API

| Method | Path |
| --- | --- |
| `GET` | `/api/workspaces/{id}/cases` |
| `POST` | `/api/workspaces/{id}/cases` |
| `GET` | `/api/workspaces/{id}/cases/{case_id}` |
| `POST` | `/api/workspaces/{id}/cases/{case_id}/observe` |
| `POST` | `/api/workspaces/{id}/cases/{case_id}/cancel` |
| `POST` | `/api/workspaces/{id}/cases/{case_id}/decide` |

## Chat tools

- `start_case` — title + `policy_md` (+ optional kind/stage/context/wake_in_s)
- `list_cases` / `get_case`
- `observe_case` — inject a fact and wake
- `cancel_case`

## Status

`open` → `waiting` → `deciding` → (`acting` later) → `completed` | `failed` | `cancelled`

`stage` is a **free string** (e.g. `awaiting_reply`, `confirm_sent`, `booked`).

## Background

`fleet-server` wakes due cases every `CASE_WAKE_INTERVAL_S` (default 15).

| Env | Default | Meaning |
| --- | --- | --- |
| `CASE_AUTO_DECIDE` | on | Claim due cases and apply decisions |
| `CASE_LLM_DECIDE` | off | Reserved for LLM JSON decisions (heuristic used today) |
| `CASE_WAKE_INTERVAL_S` | 15 | Poll interval |

## Example policy (no email integration yet)

```
kind: outreach
policy: If the user observes a reply, note it and stage=replied.
If they observe a confirmed time, stage=booked and complete.
Otherwise wait 1 hour between checks.
```

Human: `observe_case` “Jane said Thursday 3pm works” → engine advances stage.
When Gmail/Calendar tools exist, decisions can call `use_tool` without changing
this case engine.

## Schema

Migration `007_cases.sql`: tables `cases`, `case_events`.
