# Sandbox lab — per-task driver + Redis + harness (ACP)

```
  host                         per-task sandbox (compose project)
 ──────                       ──────────────────────────────────
  run-invoices-*.sh ──►        redis (internal) + workspace + driver
                               own volume /workspace  (not shared)
                               jobs/{job_id}/repo + fleet-wsexec
```

**Rule:** one task → one Compose project → one volume → one workspace.  
Do not share `/workspace` across tasks. Driver code lives in `crates/fleet-driver/`.

Architecture: [`docs/architecture/sandbox-workspace.md`](../../../docs/architecture/sandbox-workspace.md)

## Prerequisites

1. Docker + compose  
2. Harness images: `./../agents/build.sh` (opencode, pi, …)  
3. LM Studio / OpenAI-compatible server reachable from containers  
4. `cp -n .env.example .env` (see sample vars below)

## Env sample

```bash
OPENCODE_PROVIDER_ID=lmstudio
OPENCODE_BASE_URL=http://10.77.0.2:1234/v1
OPENCODE_API_KEY=lm-studio
MODEL=lmstudio/google/gemma-4-e4b
SSH_KEY_HOST_PATH=/tmp/fleet-lab-ssh/id_ed25519
```

## Working example: 123-invoices

Prefer the helper scripts (they create a **per-task** compose project + volume):

```bash
./scripts/run-invoices-example.sh config/job.123-invoices.opencode.json
./scripts/run-invoices-example.sh config/job.123-invoices.pi.json
./scripts/run-invoices-dual.sh    # both sandboxes sequential
# quiet default; VERBOSE=1 FULL_TRANSCRIPT=1 for detail
# optional non-Django:
./scripts/run-invoices-example.sh config/job.minimal-node.json
```

What happens (per task):

1. New sandbox: redis + workspace + volume for this task only  
2. Workspace clones 123-invoices, pip install, migrate, smoke check  
3. Harness (OpenCode or Pi) edits under path allowlist; tools via `fleet-wsexec`  
4. Compact result + transcript; sandbox torn down (unless `KEEP_SANDBOX=1`)

## Job fields (short)

| Field | Meaning |
|-------|---------|
| `harness` | `opencode` \| `pi` \| `codex` \| `grok` \| `custom` |
| `repo_url` / `git_ref` | clone target |
| `prompt` | task text for `session/prompt` |
| `model` | `provider_id/model_id` |
| `provider` | simplified custom provider (npm, base_url, models, api_key_env) |
| `opencode` | full `opencode.json` object (wins over `provider`) |
| `auto_approve` | default **false** (dangerous if true) |
| `permission_allow` | ACP allowlist; empty = deny; path prefixes (e.g. `app/`) gate writes; avoid `"*"` |
| `project_setup` | workspace commands, smoke, **named `tools`** |
| `work_dir` | optional; default `/workspace/jobs/{job_id}/repo` |

### Named workspace tools

```json
"project_setup": {
  "commands": ["python -m pip install -r requirements.txt"],
  "smoke_commands": ["python manage.py check"],
  "tools": {
    "django_check": { "argv": ["python", "manage.py", "check"] }
  }
}
```

Agent uses: `fleet-wsexec django_check` (workspace runs fixed argv only).

## Sample jobs

| File | Purpose |
|------|---------|
| `config/job.123-invoices.opencode.json` | OpenCode: marker + smoke test |
| `config/job.123-invoices.pi.json` | Pi: Animal.notes + migration + test |
| `config/job.123-invoices.json` | Alias of the OpenCode marker job |
| `config/job.minimal-node.json` | Non-Django tools shape |
| `config/job.deny-default.example.json` | Empty allowlist / default-deny |
| `config/job.lmstudio.example.json` | LM Studio provider sketch |
| `config/job.example.json` | Generic template |

## Tests (no LLM)

```bash
# cargo + workspace_agent handle_tool protocol (unknown / ok / cwd escape)
./scripts/run-protocol-tests.sh /tmp/fleet-sandbox-tests.log

# compose volume protocol: named tool ok + rejects (requires Docker)
./scripts/run-compose-tool-integration.sh /tmp/compose-integration.log
```

## Layout

```
crates/fleet-driver/           # everything driver-related
  src/                         # Rust binary (policy, provision, ACP)
  agents/                      # harness images (opencode/pi/codex/grok)
  sandbox-lab/
    docker-compose.yml         # per-task: redis + workspace + driver
    driver/Dockerfile
    workspace/workspace_agent.py
    config/                    # opencode + pi invoices jobs, …
    scripts/run-invoices-example.sh
    scripts/run-invoices-dual.sh
    scripts/run-protocol-tests.sh
```
