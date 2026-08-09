# fleet-driver

Process worker that **drives a coding agent over ACP** and reports to the
orchestrator using the existing `FLEET_*` / heartbeat / status contract.

Drop-in for `PROCESS_WORKER_BIN` when `SANDBOX_BACKEND=process`.
Does **not** use Docker Compose or Redis.

| Harness | How | Tokens |
| --- | --- | --- |
| **`mock`** (default) | `mock-acp-agent` stdio ACP | none — CI / demos |
| **`grok`** | `grok agent [--always-approve] [-m MODEL] stdio` | Grok Build login / `GROK_DEPLOYMENT_KEY` |

## Layout

| Path | Role |
| --- | --- |
| `src/main.rs` | Worker loop (HTTP + ACP) |
| `src/acp.rs` | JSON-RPC ACP client (newline JSON; auto-detects `Content-Length`) |
| `src/bin/mock_acp_agent.rs` | Fake ACP agent for tests |
| `src/harness.rs` | `mock` / `grok` argv |
| `src/prompt.rs` | `prompt.md` / `spec.md` / `task.json` → prompt |

## Tests (no LLM)

```bash
cargo test -p fleet-driver
```

## Run with the orchestrator

```bash
cargo build -p fleet-driver --bins
export SANDBOX_BACKEND=process
export PROCESS_WORKER_BIN="$PWD/target/debug/fleet-driver"
export FLEET_HARNESS=mock          # or grok
# export FLEET_MODEL=…             # grok -m
# export FLEET_TASK_TIMEOUT_S=600
cargo run -p fleet-server
```

Then Fleet UI → Start run → Run until done. Artifacts:

```
$DATA_DIR/projects/<id>/sessions/<session>/artifacts/
  transcript.md
  result.json
```

`mock-acp-agent` is resolved next to `fleet-driver` (same `target/debug/`), or set
`FLEET_MOCK_ACP_BIN`.

On spawn the orchestrator writes `spec.md`, `task.json`, and `prompt.md` into the
session dir, and injects `FLEET_TASK_TIMEOUT_S` (board timeout) plus any
`FLEET_HARNESS` / `FLEET_MODEL` / `FLEET_AUTO_APPROVE` set on the orch process.

### Grok Build

Requires `grok` on `PATH` and existing CLI auth (`grok login` or
`GROK_DEPLOYMENT_KEY`). The driver does **not** change `HOME`, so host auth
works under the process sandbox.

```bash
export FLEET_HARNESS=grok
export FLEET_AUTO_APPROVE=1        # default
# export FLEET_HARNESS_CMD='docker run --rm -i agent-fleet-grok:alpine'
```

## Env

| Variable | Default | Meaning |
| --- | --- | --- |
| `FLEET_*` | (required) | Same as `fleet-worker` (session, token, API URL, work dir) |
| `FLEET_HARNESS` | `mock` | `mock` \| `grok` |
| `FLEET_MODEL` | unset | Passed as `grok agent -m` |
| `FLEET_TASK_TIMEOUT_S` | `3600` | ACP wall clock |
| `FLEET_HEARTBEAT_INTERVAL_S` | `2` | Must stay `<` orch `heartbeat_stale_s` |
| `FLEET_AUTO_APPROVE` | `1` | ACP permission requests |
| `FLEET_HARNESS_CMD` | unset | Full argv override |
| `FLEET_MOCK_ACP_BIN` | sibling / PATH | Mock agent binary |

Keep using `fleet-worker` for orchestrator fixture tests that must not speak ACP.
