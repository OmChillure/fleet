# Local multi-agent fleet (no k8s)

Hermes Kanban / Vibe Kanban / OpenHands pattern, on one host:

| Piece | Behavior |
| --- | --- |
| Board | Postgres tasks + runs (durable) |
| Dispatcher | Tick loop + **background ticker** in `fleet-server` |
| Capacity | `FLEET_MAX_AGENTS` concurrent (default 16, max 256) |
| Queue | Up to **1000 tasks** per plan; extras wait for a free slot |
| Isolation | Per-session workdir; optional `FLEET_GIT_SOURCE` → `git worktree` at `repo/` |
| Secrets | Control-plane proxy only — never in the sandbox |

Chat: small plans (≤8 tasks) wait until done in the HTTP turn. Larger plans start the first wave and return; the ticker drains the rest. Watch Fleet / Sandboxes / `get_fleet_status`.

```bash
# .env
SANDBOX_BACKEND=process   # or docker; local = bash fences via fleet_worker.sh
# SANDBOX_BACKEND=local
FLEET_MAX_AGENTS=16       # hard ceiling; UI Settings sets the live count
FLEET_GIT_SOURCE=/path/to/repo   # optional
TICK_INTERVAL_S=5
# Grok workers only (host/env — not in Settings):
# FLEET_AGENT_BACKEND=grok
```

`local` / `docker` workers run ```bash``` from the task spec. `process` is the fleet-worker binary.

## True multi-agent (personas + LLM loop)

Workers use **the same provider + model as the chat composer** (Grok today; Claude later when added). Not a random Ollama tag.

Settings still has:

- **Number of agents** — concurrent sandboxes
- **Fleet LLM (Ollama)** — fallback only if a run has no chat model (fixtures)

Grok host auth stays `grok login` / `XAI_API_KEY`. `FLEET_AGENT_BACKEND=grok` is optional fallback when launching outside chat.

Each spawned task:

1. Resolves `worker_profile` to a [agency-agents](https://github.com/msitarzewski/agency-agents) persona
2. Writes `PERSONA.md` + `task.md` + live **`TEAM.md`** (who is doing what) into an isolated workdir
3. Copies finished teammates’ `result.md` into `handoff/<id>.md` (survives session wipe)
4. Runs an LLM tool loop **on the control plane** (keys never enter the box): `list_dir`, `read_file`, `write_file`, `run_bash`, `read_team`
5. Writes `result.md` + `stdout.log` so the next specialist can pick up

They do not chat peer-to-peer. They share a board: statuses + handoffs. Parallel agents see teammates as `in progress`; dependent agents wait, then read the handoff.

Chat: `list_agent_personas` then `launch_agent_plan` with different `worker_profile` per task (`frontend`, `reviewer`, `engineering-backend-architect`, …).

Aliases: `coding`, `frontend`, `reviewer`, `testing`.

Roster is cloned on first use into `DATA_DIR/agency-agents`. UI: `/{workspace}/agents`.
