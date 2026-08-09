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
  overlays/kind/               # NodePorts for kind extraPortMappings
  samples/fleetsandbox.yaml    # example CR
  kind.yaml                    # kind cluster + host ports 8080/3030
  Makefile
```

Operator source: [`crates/fleet-operator`](../crates/fleet-operator/).

## Prereqs

- Docker, `kubectl`, `kind` (or any cluster + StorageClass)
- Images built from the repo Dockerfiles

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

## Orchestrator hookup (next)

Today the tick loop uses `SANDBOX_BACKEND=process` inside the API pod. A later
`SANDBOX_BACKEND=k8s` adapter should create/delete `FleetSandbox` CRs instead
of exec'ing `fleet-worker` on the API node. The CRD + operator are the hosting
half of that path.

## Uninstall

```bash
make -C infra uninstall
kind delete cluster --name agent-fleet
```
