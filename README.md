# Agent Fleet

TUI control plane for running coding agents (**Codex**, **OpenCode**, **Grok Build**, …) over **ACP** (Agent Client Protocol).

All configuration and run history is stored in **SQLite**.

## Run

```bash
cargo run --release
# optional:
cargo run -- --db ./fleet.db
cargo run -- --print-db-path
```

Default database (macOS):

`~/Library/Application Support/agent-fleet/fleet.db`

## Features

- **Projects** — folders (`cwd`) to run agents against
- **Harness profiles** — per-agent command, model, skills, MCP servers, system prompt, env, auto-approve
- **Tasks** — title + prompt bound to a project + profile
- **ACP runs** — spawn agent over stdio, `initialize` → `session/new` → optional model config → `session/prompt`, stream log
- **SQLite** — projects, profiles, tasks, task_runs, app_meta

## Keyboard

| Key | Action |
|-----|--------|
| `Tab` / `1`–`4` | Switch pane |
| `j` / `k` | Move selection |
| `n` | New project / profile / task (depends on focused pane) |
| `e` / `Enter` | Edit selected (all fields) |
| `m` | **Set model** for the selected profile (picker + custom id) |
| `s` | Save form |
| `t` | Profile form: apply harness template |
| `r` | Run selected task |
| `d` | Delete selected item |
| `R` | Reload from SQLite |
| `q` | Quit |

## Configuration (no mock data)

The app starts with an **empty** SQLite database. You create everything yourself:

| Key | Action |
|-----|--------|
| `n` | New item in the focused pane (project / profile / task) |
| `e` / `Enter` | Edit selected item (full form) |
| `s` | Save form |
| `t` | In profile form: apply harness command template (grok/opencode/codex) |
| `d` | Delete selected |
| `r` | Run selected task over ACP |

### Profile fields

name, harness (`codex`/`opencode`/`grok`/`custom`), command, args, model, system prompt, skills JSON, MCP JSON, env JSON, auto-approve.

### Harness templates (`t` in profile form)

| Harness | Command |
|---------|---------|
| opencode | `opencode acp` |
| codex | `npx -y @agentclientprotocol/codex-acp` |
| grok | `grok agent stdio` |

## Configure MCP / skills / prompt

Profiles store JSON in SQLite. Example:

```sql
UPDATE harness_profiles
SET
  model = 'your-model-id',
  system_prompt = 'Be careful and concise.',
  skills_json = '[{"path":"/path/to/skill/SKILL.md","enabled":true}]',
  mcp_json = '[{"name":"fs","command":"/path/mcp","args":["--stdio"],"env":{}}]',
  auto_approve = 1
WHERE name = 'OpenCode (ACP)';
```

See [FLEET_TUI.md](./FLEET_TUI.md) for protocol details.

## Layout

```
src/
  main.rs      # entry + event loop
  app.rs       # TUI state & actions
  ui.rs        # ratatui drawing
  db.rs        # SQLite + seed
  models.rs    # domain types
  acp.rs       # stdio ACP client
```

## Notes

- Agents are spawned with **piped** stdio (the TUI owns the real TTY).
- Prefer each harness’s **ACP adapter** binary (e.g. `opencode acp`, `@agentclientprotocol/codex-acp`), not the interactive full-screen TUI of the agent itself.
- Grok Build ACP: `grok agent stdio` (built-in). Auth uses `authenticate` with `cached_token` or `xai.api_key` when `XAI_API_KEY` is set. See [xAI headless/scripting docs](https://docs.x.ai/build/cli/headless-scripting).
