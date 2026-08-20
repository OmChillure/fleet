# Cases — durable general-purpose workflows

Cases are the **generalized platform layer**: long-lived work with backend-owned
state. Coding agent fleets (orchestrator runs) are an **optional executor**.
Integrations (Slack, GitHub, Notion, …) run via `use_tool` → control-plane
`integration_query`. Coding fleets are an **optional executor**.

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
| `CASE_LLM_DECIDE` | on if Grok or Ollama is enabled; set `0` to force heuristic | JSON decide via Grok, else Ollama |
| `CASE_WAKE_INTERVAL_S` | 15 | Poll interval |

`use_tool` calls the same proxy as chat (`integration_query`). Tokens stay on
the control plane. `launch_agents` accepts a plan, sets `status=acting` +
`run_id`, and the wake loop resumes the case when that fleet run is terminal.
The wake payload includes task statuses and shared `work/` file names so the
next `use_tool` (GitHub PR / Notion) can see what shipped.

UI: `/{workspace_id}/cases` and `/{workspace_id}/cases/{case_id}`.

## Example policy (no email integration yet)

```
kind: outreach
policy: If the user observes a reply, note it and stage=replied.
If they observe a confirmed time, stage=booked and complete.
Otherwise wait 1 hour between checks.
```

Human: `observe_case` “Jane said Thursday 3pm works” → engine advances stage.
The LLM can also `use_tool` (Slack history, GitHub PR, Notion page) without a
hardcoded playbook.

## Schema

Migration `007_cases.sql`: tables `cases`, `case_events`.
