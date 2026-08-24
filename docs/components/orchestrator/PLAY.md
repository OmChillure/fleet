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
2. Sidebar → **Settings** → concurrent agents + optional Ollama fallback  
3. Sidebar → **Fleet**  
4. **Start 2-task run** → **Run until done** (or **Run tick** a few times)  

Agents share a run workspace (`work/`). They do not clone a Settings git URL. If you want a PR on GitHub, connect GitHub under **Integrations** and ask Chat — `integration_query` with `provider=github`.

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
# Grok Build (host CLI auth):
# export PATH="$HOME/.grok/bin:$PATH"
# export FLEET_HARNESS=grok
# export FLEET_REASONING_EFFORT=low
# export FLEET_MODEL=v9
export API_PUBLIC_BASE=http://127.0.0.1:8080
cargo run -p fleet-server
```

Sessions write `spec.md`, `task.json`, `prompt.md`, optional `workspace.json`, plus `artifacts/transcript.md` and `artifacts/result.json`. The cloned tree (if configured) is `repo/`. See [`crates/fleet-driver/README.md`](../../../crates/fleet-driver/README.md).

### Kubernetes (`FleetSandbox` CRs)

Tick creates CRs; [`fleet-operator`](../../../crates/fleet-operator/) turns each into a pod. Cluster from [`infra/README.md`](../../../infra/README.md) (k3d or kind). Do **not** point workers at `http://127.0.0.1:8080` — the pod cannot reach host loopback.

```bash
export SANDBOX_BACKEND=k8s
export K8S_NAMESPACE=agent-fleet
export K8S_WORKER_IMAGE=agent-fleet-api:local
export K8S_WORKER_COMMAND=fleet-worker
# In-cluster API (the API ServiceAccount also needs FleetSandbox create/delete):
export K8S_WORKER_API_URL=http://api.agent-fleet.svc.cluster.local:8080
# Host API + cluster (ports 8080/3030 must stay on the host, not kind extraPortMappings).
# Use a host IPv4/IPv6 literal — allowlisted /32 or /128. DNS names are not CIDRs.
# export K8S_WORKER_API_URL=http://172.18.0.1:8080
cargo run -p fleet-server
```

Needs `KUBECONFIG` or in-cluster config, the CRD, and the operator. Missing client/CRD is a spawn **fail**, not a stub fallback. Spec is passed as `FLEET_SPEC_MD` (max ~200KiB). SessionDetail tails host `DATA_DIR`; pod artifacts stay in the pod `emptyDir` until a later pull.

Optional: set `ADMIN_TOKEN` to enable `/v1/*` bearer automation (`./scripts/demo-orch.sh`).
