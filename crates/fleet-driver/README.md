# fleet-driver

In-sandbox driver: provision a **workspace**, run a coding **harness over ACP**, report on Redis.

**Everything driver-related is in this crate** — binary, harness images, lab compose, jobs, scripts.  
Architecture: [`docs/architecture/sandbox-workspace.md`](../../docs/architecture/sandbox-workspace.md).

## Layout

| Path | Role |
| --- | --- |
| `src/` | `fleet-driver` binary (provision, policy, ACP client, OpenCode/Pi config) |
| `tests/` | Cargo fixture tests (job JSON under `sandbox-lab/config`) |
| `agents/` | Alpine harness images: base, opencode, pi, codex, grok + `build.sh` |
| `sandbox-lab/` | Per-task compose lab: redis, workspace agent, driver image, jobs, scripts |

## Isolation model

**One task = one sandbox:**

- unique Compose project (`fleet-task-<id>`)
- **own Docker volume** (`…_sandbox-workspace`) — tasks never share `/workspace`
- own workspace container + redis (no host redis port; talk via `compose exec`)
- own driver run (harness image via `DRIVER_BASE_IMAGE`)

Torn down after the script finishes (`KEEP_SANDBOX=1` to keep).  
Git **branches are not** created here — branching / PR is orchestrator-side later.

```
task A → fleet-task-aaa + volume A + workspace A + driver(opencode)
task B → fleet-task-bbb + volume B + workspace B + driver(pi)
```

## Quick tests (no LLM)

```bash
# from repo root
cargo test -p fleet-driver
./sandbox-lab/scripts/run-protocol-tests.sh /tmp/fleet-sandbox-tests.log

# optional Docker protocol (no LLM)
cd sandbox-lab && ./scripts/run-compose-tool-integration.sh /tmp/compose-int.log
```

## Harness images

```bash
# from repo root
./crates/fleet-driver/agents/build.sh
# or: cd crates/fleet-driver/agents && ./build.sh
```

| Image | Entry | Notes |
| --- | --- | --- |
| `agent-fleet-opencode:alpine` | `opencode acp` | default lab driver base |
| `agent-fleet-pi:alpine` | `pi-acp` | [pi.dev](https://pi.dev) + local models via `~/.pi/agent/models.json` |
| `agent-fleet-codex:alpine` | `codex-acp` | |
| `agent-fleet-grok:alpine` | `grok agent stdio` | |

Lab sets `DRIVER_BASE_IMAGE` automatically for pi jobs.

## Invoices e2e

Needs Docker + OpenAI-compatible API (e.g. LM Studio).

### Env (`sandbox-lab/.env`)

```bash
cp -n sandbox-lab/.env.example sandbox-lab/.env
```

Working sample:

```bash
OPENCODE_PROVIDER_ID=lmstudio
OPENCODE_BASE_URL=http://10.77.0.2:1234/v1
OPENCODE_API_KEY=lm-studio
MODEL=lmstudio/google/gemma-4-e4b
SSH_KEY_HOST_PATH=/tmp/fleet-lab-ssh/id_ed25519
```

| Var | Required | Notes |
| --- | --- | --- |
| `OPENCODE_BASE_URL` | yes | Reachable **from the driver container** (LAN IP or `host.docker.internal`, not host `127.0.0.1`) |
| `MODEL` | yes | `provider_id/model_id` (same for OpenCode and Pi) |
| `OPENCODE_API_KEY` | yes | Dummy OK for many local servers |
| `SSH_KEY_HOST_PATH` | yes | Mounted into driver (placeholder fine for HTTPS clone) |

### Jobs

| Job file | Harness | Task | Tools (required → optional) | Edit allowlist |
| --- | --- | --- | --- | --- |
| `config/job.123-invoices.opencode.json` | opencode | `app/fleet_marker.py` + `tests/test_fleet_smoke.py` | check → migrate → `django_test_fleet` → `django_test` | `app/`, `tests/` |
| `config/job.123-invoices.pi.json` | pi | `Animal.notes` field + migration + test | makemigrations → migrate → check → `django_test_fleet` → `django_test` | `invoices/`, `tests/` |
| `config/job.123-invoices.json` | opencode | alias of the marker job | same as opencode | `app/`, `tests/` |

- `auto_approve=false`, **no** `permission_allow: "*"`
- Path-like allow entries gate **file writes**; `fleet-wsexec` / `bash` / `read` for tools
- Full suite timeout/fail is OK to report (stretch)

### Run

```bash
cd sandbox-lab   # or from repo: crates/fleet-driver/sandbox-lab

# quiet by default
./scripts/run-invoices-example.sh config/job.123-invoices.opencode.json
./scripts/run-invoices-example.sh config/job.123-invoices.pi.json

# both sandboxes sequential (same MODEL)
./scripts/run-invoices-dual.sh
```

| Flag | Effect |
| --- | --- |
| `VERBOSE=1` | Docker + full driver logs |
| `FULL_TRANSCRIPT=1` | Full model transcript (default ~1200 chars) |
| `TRANSCRIPT_CHARS=N` | Truncation length |
| `KEEP_SANDBOX=1` | Skip teardown (inspect volume) |
| `DRIVER_BASE_IMAGE=…` | Override harness base image |

Example output (quiet):

```text
task  job=…  harness=opencode  model=lmstudio/…  project=fleet-task-…
driver…
  INFO … provision complete …
  INFO … ACP prompt complete …
  INFO … published result …
result
  ok=true  harness=opencode  model=…
  elapsed=…s  stop=end_turn
  tools=django_check, django_migrate, …
  files: app/fleet_marker.py, tests/test_fleet_smoke.py, …
  --- transcript ---
  …
done
```

## Policy notes

- Default-deny ACP permissions; substring allowlist + **path-gated writes** (`policy.rs`)
- Workspace tools: fixed argv in `.fleet-tools.json` only (`fleet-wsexec <name>`)
- Job workdir: `/workspace/jobs/{job_id}/repo` under the **task’s** volume
