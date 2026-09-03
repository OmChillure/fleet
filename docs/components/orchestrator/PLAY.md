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

### Process guest (real worker)

The API does not spawn `fleet-driver`. Pair a `fleet-host`; that process
starts the guest. Workers call back on `API_PUBLIC_BASE` (must not be
`0.0.0.0`).

```bash
export API_PUBLIC_BASE=http://127.0.0.1:8080
cargo run -p fleet-server
# Add worker in the UI, then install + pair fleet-host on the machine.
```

Each leased session writes `spec.md` plus `artifacts/result.json` under the host work dir.

### ACP driver (mock / Grok Build)

`fleet-driver` speaks ACP. Default harness is mock; set `FLEET_HARNESS=grok` on the **host** for Grok Build. See [`crates/fleet-driver/README.md`](../../../crates/fleet-driver/README.md).

### Kubernetes (`FleetSandbox` CRs)

Tick enqueues a `work_claims` row; in-cluster [`fleet-host`](../../../crates/fleet-host/) creates the CR. [`fleet-operator`](../../../crates/fleet-operator/) turns each into a pod. Cluster from [`infra/README.md`](../../../infra/README.md) (k3d or kind). Do **not** point workers at `http://127.0.0.1:8080` — the pod cannot reach host loopback.

In-cluster (kind/k3d): the API is not a runner; `fleet-host` bootstrap-pairs
and creates/deletes CRs. Start a Fleet run after a workspace exists;
`kubectl -n agent-fleet get fsb` should show a CR. SessionDetail stays empty
(no pod artifact pull yet).

Needs `KUBECONFIG` or in-cluster config, the CRD, and the operator. Missing client/CRD is a spawn **fail**, not a stub fallback. Spec is passed as `FLEET_SPEC_MD` (max ~200KiB). SessionDetail tails host `DATA_DIR`; pod artifacts stay in the pod `emptyDir` until a later pull.

Optional: set `ADMIN_TOKEN` to enable `/v1/*` bearer automation (`./scripts/demo-orch.sh`).
