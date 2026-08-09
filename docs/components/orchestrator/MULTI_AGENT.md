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
SANDBOX_BACKEND=local     # bash fences via fleet_worker.sh; or docker
# SANDBOX_BACKEND=process # fleet-worker binary (PROCESS_WORKER_BIN), not bash fences
FLEET_MAX_AGENTS=16
FLEET_GIT_SOURCE=/path/to/repo   # optional
TICK_INTERVAL_S=5
```

Not yet (needs k8s / E2B / a real harness): 1000 *live* containers, Claude Code/Grok CLI inside the box. `local` / `docker` workers run ```bash``` from the task spec. `process` is the fleet-worker binary.
