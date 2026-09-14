# Local multi-agent fleet (no k8s)

Hermes Kanban / Vibe Kanban / OpenHands pattern, on one host:

| Piece | Behavior |
| --- | --- |
| Board | Postgres tasks + runs (durable) |
| Dispatcher | Tick loop + **background ticker** in `fleet-server` |
| Capacity | `FLEET_MAX_AGENTS` concurrent (default 16, max 256) |
| Queue | Up to **1000 tasks** per plan; extras wait for a free slot |
| Isolation | Per-session workdir + shared `work/` for the run (no git URL required) |
| Secrets | Control-plane proxy only — never in the sandbox |

Chat: small plans (≤8 tasks) wait until done in the HTTP turn. Larger plans start the first wave and return; the ticker drains the rest. Watch Fleet / Sandboxes / `get_fleet_status`.

```bash
# .env — API is not a runner. Pair fleet-host for real jobs.
FLEET_MAX_AGENTS=16       # hard ceiling; UI Settings sets the live count
TICK_INTERVAL_S=5
# Grok workers only (host/env — not in Settings):
# FLEET_AGENT_BACKEND=grok
```

Paired `fleet-host` starts `fleet-driver`. Isolation is on that machine.

## True multi-agent (personas + LLM loop)

Guests use **the same provider + model as the chat composer** (any supported provider on that runner). Not a random Ollama tag.

Settings still has:

- **Number of agents** — concurrent sandboxes
- **Fleet LLM (Ollama)** — fallback only if a run has no chat model (fixtures)

Grok host auth stays `grok login` / `XAI_API_KEY`. Workspace `llm-keys` win when present; env is fallback. `FLEET_AGENT_BACKEND=grok` is optional fallback when launching outside chat.

Each spawned task:

1. Resolves `worker_profile` to a [agency-agents](https://github.com/msitarzewski/agency-agents) persona
2. Writes `PERSONA.md` + `task.md` + live **`TEAM.md`** (who is doing what) into an isolated workdir
3. Copies finished teammates’ `result.md` into `handoff/<id>.md` and their **files** into `handoff/<id>/` plus `work/` (shared run workspace)
4. Runs the official CLI **on the paired runner** (`fleet-host` → `fleet-driver`, same family as chat: Grok / Claude / Ollama). The control plane does not call a model. Team context (`TEAM.md`, `work/`, handoffs) is packed into the `spawn_session` claim. Integrations and team chat go back through `POST /v1/.../workers/{id}/io` (or `./fleet-tool.sh`). Keys stay in that box's `grok login` / Claude / Ollama and never enter the claim payload. Write `result.md` when done.
5. Writes `result.md` + `stdout.log` so the next specialist can pick up
6. **Done gate:** process/local/docker tasks are not `done_success` until `result.md` exists and acceptance (`exists:`, `contains:`) passes. Fail → task goes back to `ready` with notes. Stub fixtures skip the file checks.

**They share a repo and they talk.** Isolated session dirs still exist, but:

- `runs/{run}/workspace/` is the canonical tree (empty shared folder — not a GitHub clone)
- Each agent works in `work/` and successful trees sync back + archive under `handoffs/{id}/tree/`
- Reviewers call `request_changes` → implementer goes back to `ready` with notes; reviewer waits again

Live chat (still needed for “what port *now*”):

`DATA_DIR/projects/{workspace}/runs/{run}/chat.jsonl`

- `team_chat_post` — `kind` `say` (FYI) or `ask` (wait for a reply) to one teammate id or `*`
- `team_chat_list` — read messages (`all` or unread)
- Unread messages are **injected** into the other agent’s next LLM round (not only if they poll)
- Human chat tools may alias these names; `/io` accepts `send_message` / `read_messages` as aliases

Handoffs + the board still matter for finished work. Chat is for “I need your port / contract *now*.” Parallel agents see teammates as `in progress` and can ask them; dependent agents wait, then read the handoff **and** the transcript.

Chat: `list_agent_personas` then `launch_agent_plan` with different `worker_profile` per task. Omit the profile for **generalist**. Optional shortcuts: `coding`, `frontend`, `reviewer`, `testing`.

If a live sandbox is the wrong specialist, the Fleet **Replace** button posts
`POST /api/workspaces/{id}/fleet/sessions/{session_id}/replace`. There is no
`swap_agent` chat tool. Cap: two replacements per task.

Cross-app work is a normal chat sentence. The assistant calls `integration_query` (Slack/Notion/GitHub/…) then `launch_agent_plan`; workers use `use_integration`; plan `outputs` write back when the run ends.

Roster is cloned on first use into `DATA_DIR/agency-agents`. UI: `/{workspace}/agents`.
