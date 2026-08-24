# Infra — Kubernetes control plane + sandbox operator

Installs Agent Fleet on Kubernetes:

| Piece | What |
| --- | --- |
| **CRD** `FleetSandbox` (`fsb`) | Desired worker sandbox (session + image + network + TTL) |
| **fleet-operator** | Reconciles each CR → Pod + default-deny NetworkPolicy |
| **Control plane** | Postgres, `fleet-server` (api), `fleet-ui`, operator |

This is the **Sandbox Hosting Tool** for the `k8s` backend described in
[`docs/architecture/README.md`](../docs/architecture/README.md) and
[`docs/components/sandbox-hosting-tool/`](../docs/components/sandbox-hosting-tool/).

Sandboxes start with **no general internet**: egress is kube-dns + `api:8080` only
(`spec.network.egressMode: none`). Add CIDRs via `allowlist` when needed.
Secrets never belong in the sandbox CR — pass worker tokens as env from the
orchestrator, not long-lived cloud keys.

## Layout

```
infra/
  crds/fleetsandboxes.yaml     # FleetSandbox CRD
  base/                        # namespace, postgres, api, ui, operator, RBAC, NPs
  overlays/k3d/                # NodePorts for k3d loadbalancer
  overlays/kind/               # NodePorts for kind extraPortMappings
  samples/fleetsandbox.yaml    # example CR
  k3d.yaml                     # k3d cluster + host ports 8080/3030
  kind.yaml                    # kind cluster + host ports 8080/3030
  Makefile
```

Operator source: [`crates/fleet-operator`](../crates/fleet-operator/).

## Prereqs

- Docker, `kubectl`
- **k3d** (preferred local) or `kind`
- Images built from the repo Dockerfiles (`make images` passes `FLEET_GIT_SHA`)

## k3d quickstart (recommended)

```bash
# from repo root — 1 server, no agents, Traefik disabled
brew install k3d            # once
make -C infra k3d-create
make -C infra k3d-load      # docker build + import into the cluster
make -C infra apply-k3d

kubectl -n agent-fleet get pods
```

Host ports (see `infra/k3d.yaml`):

- UI  http://localhost:3030
- API http://localhost:8080

```bash
kubectl -n agent-fleet apply -f infra/samples/fleetsandbox.yaml
kubectl -n agent-fleet get fsb,pods -l app.kubernetes.io/component=sandbox
```

Tear down:

```bash
make -C infra k3d-delete
```

## kind quickstart

```bash
# from repo root
make -C infra kind-create
make -C infra kind-load
make -C infra apply-kind

kubectl -n agent-fleet get pods -w
# then
kubectl -n agent-fleet apply -f infra/samples/fleetsandbox.yaml
kubectl -n agent-fleet get fsb,pods -l app.kubernetes.io/component=sandbox
```

kind `extraPortMappings` publish:

- API http://localhost:8080
- UI  http://localhost:3030

Without kind, `kubectl apply -k infra/` and port-forward:

```bash
kubectl -n agent-fleet port-forward svc/ui 3030:3030
kubectl -n agent-fleet port-forward svc/api 8080:8080
```

## CRD

```bash
make -C infra print-crd          # stdout
make -C infra crd                # rewrite infra/crds/fleetsandboxes.yaml
```

### FleetSandbox spec (abbrev.)

| Field | Notes |
| --- | --- |
| `workspaceId` / `projectId` / `sessionId` | Orchestrator identity |
| `image` | Usually `agent-fleet-api:local` (includes `fleet-worker`) |
| `command` | Default `["fleet-worker"]` |
| `env` | `FLEET_*` contract (`FLEET_WORKER_TOKEN`, …) |
| `resources.cpu/memory/ephemeralStorage` | Pod limits=requests |
| `network.egressMode` | `none` (default) or `allowlist` |
| `network.allowlist` | CIDRs only (`10.0.0.0/8`) |
| `ttlSeconds` | Operator deletes the CR after this age |

Status: `phase` (`Creating` / `Running` / `Succeeded` / `Failed`), `podName`, `nodeName`, `message`.

## Operator env

| Env | Default |
| --- | --- |
| `WATCH_NAMESPACE` | empty = all namespaces; install sets `agent-fleet` |
| `WORKER_IMAGE` | `agent-fleet-api:local` |
| `API_SERVICE_URL` | `http://api.agent-fleet.svc.cluster.local:8080` |
| `API_NAMESPACE` | `agent-fleet` |
| `API_POD_LABEL` | `fleet-api` (`app.kubernetes.io/name`) |

## Orchestrator hookup

`SANDBOX_BACKEND=k8s` on `fleet-server` / `fleet-orchestrator` creates and
deletes `FleetSandbox` CRs. The operator still creates the Pod + NetworkPolicy.
Worker heartbeats stay `POST /v1/.../heartbeat|status`.

| Env | Default | Role |
| --- | --- | --- |
| `SANDBOX_BACKEND` | `stub` (in-cluster API Deployment: `process`) | set `k8s`. The API ServiceAccount cannot create CRs until you bind it the same verbs as `fleet-operator`. Host `KUBECONFIG` works without that. |
| `K8S_NAMESPACE` | `agent-fleet` | CR namespace |
| `K8S_WORKER_IMAGE` | `agent-fleet-api:local` | pod image (`fleet-worker`) |
| `K8S_WORKER_COMMAND` | `fleet-worker` | container command |
| `K8S_WORKER_API_URL` | `http://api.agent-fleet.svc.cluster.local:8080` | heartbeat base — **not** `127.0.0.1` / `localhost` |

In-cluster: patch the API Deployment (it still defaults to `process`) or set
those env vars and restart. Hybrid (host API + cluster operator): keep host
`:8080`/`:3030` off the kind/k3d port map and set `K8S_WORKER_API_URL` to a
host IPv4/IPv6 literal pods can route (allowlisted `/32` or `/128`). DNS
names such as `host.k3d.internal` are not CIDRs. Missing kube client or
CRD fails spawn — no stub fallback.

v1 does not pull pod logs back to host `DATA_DIR`. The board still moves on
worker HTTP status + CR phase.

## Uninstall

```bash
make -C infra uninstall
make -C infra k3d-delete          # or: kind delete cluster --name agent-fleet
```
