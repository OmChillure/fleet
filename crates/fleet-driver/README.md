# fleet-driver

Guest of a paired `fleet-host`. Reports over the existing `FLEET_*` /
heartbeat / status contract. The control plane does **not** run the model.

| Harness | How | Tokens |
| --- | --- | --- |
| **`mock`** (default) | `mock-acp-agent` stdio ACP | none — CI / demos |
| **`grok`** | official `grok` CLI (`--prompt-file`, `--permission-mode dontAsk`) | `grok login` on the runner |
| **`claude`** | official `claude` CLI print/agent mode | Claude login on the runner |
| **`ollama`** | `ollama run <model>` | local Ollama on the runner |
| **`grok-acp`** | `grok agent stdio` (opt-in ACP) | same as Grok login |

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
# live Grok Build (uses tokens; needs grok login):
FLEET_GROK_ACP_E2E=1 cargo test -p fleet-driver grok_tiny_prompt_over_acp -- --nocapture
```

## Run with a paired host

`fleet-host` starts this binary for each leased job. Pair a host, then
Fleet UI → Start run → Run until done. Artifacts:

```
$DATA_DIR/projects/<id>/sessions/<session>/artifacts/
  transcript.md
  result.json
```

`mock-acp-agent` is resolved next to `fleet-driver` (same `target/debug/`), or set
`FLEET_MOCK_ACP_BIN`.

On spawn the orchestrator writes `spec.md`, `task.json`, `prompt.md`, and
optional `workspace.json` into the session dir, and injects `FLEET_TASK_TIMEOUT_S`
(board timeout), `FLEET_REPO_URL` / `FLEET_GIT_REF` when the project has a git
remote, plus any `FLEET_HARNESS` / `FLEET_MODEL` / `FLEET_AUTO_APPROVE` set on
the orch process. The driver clones into `{work_dir}/repo` and uses that as the
agent cwd; no remote → skip clone.

### Grok Build

Verified against `grok agent stdio` (NDJSON ACP): `initialize` → `authenticate`
(`cached_token` from `~/.grok/auth.json`) → `session/new` → `session/prompt` →
`session/close`. The driver does **not** change `HOME`, so host CLI login and
config apply. `--no-leader` avoids attaching to a TUI leader process.

```bash
export FLEET_HARNESS=grok
export FLEET_AUTO_APPROVE=1                 # default
# export FLEET_MODEL=v9                     # grok agent -m
# export FLEET_REASONING_EFFORT=low         # or medium / high
# export FLEET_GROK_BIN=$HOME/.grok/bin/grok
# export FLEET_GROK_DEBUG=1                 # work_dir/grok-acp.debug.log
# export PATH="$HOME/.grok/bin:$PATH"
```

Process sandbox uses your user `~/.grok` (including MCP servers in
`config.toml`). Isolate later via Docker/k8s if that is too much host coupling.

## Env

| Variable | Default | Meaning |
| --- | --- | --- |
| `FLEET_*` | (required) | Session, token, API URL, work dir (same contract as the pair payload) |
| `FLEET_HARNESS` | `mock` | `mock` \| `grok` \| `ollama` \| `openai` \| `gemini` \| `claude` (same ids as chat; aliases `xai`/`codex`/`google`/`anthropic`) |
| `FLEET_MODEL` | unset | `grok agent -m` (else Grok config default) |
| `FLEET_REASONING_EFFORT` | unset | `grok agent --reasoning-effort` |
| `FLEET_GROK_BIN` | `~/.grok/bin/grok` or `PATH` | Grok CLI |
| `FLEET_GROK_DEBUG` | `0` | Write `grok-acp.debug.log` in the work dir |
| `FLEET_TASK_TIMEOUT_S` | `3600` | ACP wall clock |
| `FLEET_HEARTBEAT_INTERVAL_S` | `2` | Must stay `<` orch `heartbeat_stale_s` |
| `FLEET_AUTO_APPROVE` | `1` | ACP permission requests |
| `FLEET_REPO_URL` | unset | Project clone URL (skip clone if empty) |
| `FLEET_GIT_REF` | `main` | Branch, tag, or commit after clone |

`initialize` advertises `clientCapabilities.terminal = true`. The driver implements `terminal/create|output|wait_for_exit|kill|release` so Grok can run shell commands. Returning `{}` for those methods fails with `failed to deserialize response`.
| `FLEET_HARNESS_CMD` | unset | Full argv override |
| `FLEET_MOCK_ACP_BIN` | sibling / PATH | Mock agent binary |

The guest is always `fleet-driver`. Use `FLEET_HARNESS=mock` when a test must not call a real model.

In-sandbox Compose isolation (OpenCode/Pi/volume handshake) is
[`crates/fleet-sandbox-driver`](../fleet-sandbox-driver/). Do not point
`PROCESS_WORKER_BIN` at `fleet-sandbox-driver`.
