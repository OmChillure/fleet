# fleet-operator

Kubernetes controller for **`FleetSandbox`** CRs. Turns each CR into a
non-root worker Pod plus a default-deny NetworkPolicy (DNS + control-plane API
only).

Install manifests live in [`infra/`](../../infra/).

```bash
# emit CRD YAML
cargo run -p fleet-operator -- print-crd

# run locally against KUBECONFIG (watches WATCH_NAMESPACE)
export WATCH_NAMESPACE=agent-fleet
cargo run -p fleet-operator
```
