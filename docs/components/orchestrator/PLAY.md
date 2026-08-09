# Play with Fleet (proper workspace hookup)

## Mapping

`workspace_id` **is** the orchestrator `project_id`. Opening **Fleet** ensures that project exists for your logged-in workspace.

## Run

```bash
docker compose up -d postgres

# .env (minimum)
# DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable
# JWT_SECRET=…(≥32 chars)
# DATA_DIR=/tmp/fleet-orch-data
# ADMIN_TOKEN is optional (only for /v1 scripts)

cargo run -p fleet-server          # :8080
dx serve --package fleet-ui --port 3030
```

1. Signup / login  
2. Sidebar → **Fleet**  
3. **Start 2-task run** → **Run until done** (or **Run tick** a few times)  

Cookie auth on `/api/workspaces/{id}/fleet/*`. **No ADMIN_TOKEN required.**

Fleet shows the active sandbox backend, worker sessions, and task progress.
**Sandboxes** in the sidebar lists the same sessions (work dir, backend, exit reason).

### Process sandbox (real worker)

```bash
cargo build -p fleet-orchestrator --bin fleet-worker
export SANDBOX_BACKEND=process
export PROCESS_WORKER_BIN="$PWD/target/debug/fleet-worker"
# workers call back on this URL (must not be 0.0.0.0)
export API_PUBLIC_BASE=http://127.0.0.1:8080
cargo run -p fleet-server
```

Each spawned session writes `spec.md` plus `artifacts/result.json` under `DATA_DIR/projects/<workspace>/sessions/<session>/`.

### ACP driver (mock / Grok Build)

`fleet-worker` does not speak ACP. Point the process backend at `fleet-driver` to run a coding agent (mock in CI, Grok Build on a laptop):

```bash
cargo build -p fleet-driver --bins
export SANDBOX_BACKEND=process
export PROCESS_WORKER_BIN="$PWD/target/debug/fleet-driver"
export FLEET_HARNESS=mock
# export FLEET_HARNESS=grok          # requires `grok` on PATH + grok login
# export FLEET_MODEL=…               # optional grok -m
export API_PUBLIC_BASE=http://127.0.0.1:8080
cargo run -p fleet-server
```

Sessions write `spec.md`, `task.json`, `prompt.md`, plus `artifacts/transcript.md` and `artifacts/result.json`. See [`crates/fleet-driver/README.md`](../../../crates/fleet-driver/README.md).

Optional: set `ADMIN_TOKEN` to enable `/v1/*` bearer automation (`./scripts/demo-orch.sh`).
