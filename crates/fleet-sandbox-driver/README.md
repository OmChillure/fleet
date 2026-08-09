# fleet-sandbox-driver

In-sandbox driver: provision a **workspace**, run a coding **harness over ACP**,
report on Redis. One task = one Compose project = one volume.

Local compose rig: [`sandbox-lab/`](sandbox-lab/). Harness images: [`agents/`](agents/).  
Architecture: [`docs/architecture/sandbox-workspace.md`](../../docs/architecture/sandbox-workspace.md).

This is **not** `PROCESS_WORKER_BIN`. The orch process worker is
[`crates/fleet-driver`](../fleet-driver/) (`fleet-driver`).

## Install / tests

```bash
crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh
crates/fleet-sandbox-driver/sandbox-lab/scripts/run-full-suite.sh
cargo test -p fleet-sandbox-driver
```
