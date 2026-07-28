# Agent Fleet TUI

Ratatui control plane that runs coding agents over **ACP** (Agent Client Protocol). All config and run data lives in **SQLite**.

## Quick start

```bash
cargo run -p agent-fleet-driver --release
# or
cargo install --path driver
agent-fleet
agent-fleet --print-db-path
agent-fleet --db ./fleet.db
```

Default DB: `~/Library/Application Support/agent-fleet/fleet.db` (macOS).

## Concepts

| Entity | Meaning |
|--------|---------|
| **Project** | Folder (`cwd`) the agent runs against |
| **Profile** | Harness config: command/args, model, skills, MCP servers, system prompt |
| **Task** | Prompt bound to a project + profile |
| **Run** | One ACP session execution (log stored in SQLite) |

### Seeded harness profiles

| Profile | Default command |
|---------|-----------------|
| OpenCode (ACP) | `opencode acp` |
| Codex (ACP) | `codex-acp` or `npx -y @agentclientprotocol/codex-acp` |
| Grok Build (ACP) | `grok agent stdio` |

Edit profiles directly in SQLite or via the TUI (`e` = model). MCP/skills/prompt are stored as JSON columns on `harness_profiles`.

## Keys

| Key | Action |
|-----|--------|
| `Tab` / `1-4` | Focus pane |
| `j`/`k` | Move selection |
| `n` | New task (uses selected project + profile) |
| `p` | New project |
| `e` | Edit profile model |
| `r` | Run selected task over ACP |
| `d` | Delete selected row |
| `R` | Reload from SQLite |
| `q` | Quit |

## Schema (SQLite)

- `projects` — name, path, notes  
- `harness_profiles` — command, args_json, model, system_prompt, skills_json, mcp_json, env_json, auto_approve  
- `tasks` — project_id, profile_id, title, prompt, status  
- `task_runs` — session_id, log, error, timestamps  
- `app_meta` — key/value  

## MCP / skills in a profile

```sql
UPDATE harness_profiles
SET mcp_json = '[{"name":"filesystem","command":"/path/to/server","args":["--stdio"],"env":{}}]',
    skills_json = '[{"path":"/path/to/skill-or-SKILL.md","enabled":true}]',
    system_prompt = 'Be careful with production.',
    model = 'your-model-id'
WHERE name = 'OpenCode (ACP)';
```

Skills are inlined into the prompt (file or `SKILL.md` directory). MCP servers are passed on `session/new` as `mcpServers`.

## ACP flow

1. Spawn profile command with piped stdio  
2. `initialize`  
3. `session/new` with project `cwd` + MCP list  
4. Optional `session/set_config_option` for model  
5. `session/prompt` with system + skills + task text  
6. Stream `session/update` into the log pane  
7. Auto-approve permissions when `auto_approve=1`  
