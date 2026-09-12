# Runners

One product surface for **where work runs**. Five execution kinds, one
assignment path, two isolation modes.

How the guest runs is a separate product split: **Run on a PC** vs
**Run in a container**. The five kinds remain placement/ownership;
that split is the use case. See
[`run-on-pc-vs-container.md`](run-on-pc-vs-container.md).

Self-hosted pairing, install, heartbeat, and lease stay in
[`docs/components/worker-host/README.md`](../worker-host/README.md).
This folder is the umbrella: kinds, isolation, nav, and routes.

Testing (required three layers + how to run):
[`testing-requirements.md`](testing-requirements.md),
[`testing-procedures.md`](testing-procedures.md).

---

## Five kinds

| Kind | Who owns the machine | Isolation | Status |
| --- | --- | --- | --- |
| Self-hosted native | Customer PC / server / VPS | **Native** (not a sandbox) | paired `fleet-host` |
| Self-hosted sandbox | Same machine, containers/k8s on it | Sandboxed | assignable when the host advertises container |
| Public cloud | Customer AWS / GCP / Azure keys | Sandboxed | `POST …/runners/cloud/byo` (encrypted creds + pair token) |
| Fleet Cloud | Our paid pool | Sandboxed | `POST …/runners/cloud/fleet` (pair a Fleet-hosted box) |
| Partners | Other people’s unused capacity | Sandboxed | offer / borrow / kill-switch; sandbox-only |

Tick assigns to a paired `fleet-host`. The API process is not a runner
and does not appear on Self-hosted. No host → the task stays ready.
Same-machine install is **Add worker** then pair. Do not invent a
second spawn path.

---

## Isolation is on the host (once)

Do not add Native / Sandboxed checkboxes on runner cards. How a job is
isolated is the host’s process / container / vm class (workspace
`min_isolation`). The API does not run sandboxes. Pause / “Stop taking
new jobs” is not isolation.

Sidebar: **Runners** accordion (same rank as Monitoring and Agents).
Children: **Self-hosted** (pair + list), **Cloud** (Fleet Cloud attach;
BYO cards stay labeled), **Partners** (lend / borrow after disclaimer).
Workspace keeps Integrations + Settings.

---

## Naming

Do **not** rename `fleet-host`, `fleet-driver`, or the `worker_hosts`
table. API paths stay `/api/workspaces/{id}/worker-hosts` and
`/api/worker-hosts/…`.

**Runners** is the product word (a machine that executes someone else’s
job). **Workers** as a nav section collides with the per-job
`fleet-driver` process.

---

## Routes after MR 3

| Path | Page |
| --- | --- |
| `/{workspace_id}/runners` | Redirect to `/runners/self-hosted` |
| `/{workspace_id}/runners/self-hosted` | Current Workers UI (pair + list) |
| `/{workspace_id}/workers` | Redirect to `/runners/self-hosted` |
| `/{workspace_id}/runners/cloud` | Fleet Cloud attach + BYO cards |
| `/{workspace_id}/runners/partners` | Disclaimer + lend / borrow ([design](partners.md)) |

---

## Related

| Doc | Why |
| --- | --- |
| [`testing-requirements.md`](testing-requirements.md) | Every runner behavior needs unit, e2e, and real-life VM/OS tests |
| [`testing-procedures.md`](testing-procedures.md) | How to run those three layers |
| [`docs/components/runners/partners.md`](partners.md) | Partners credit, trust, kill-switch (design) |
| [`docs/components/worker-host/README.md`](../worker-host/README.md) | Pair, install, heartbeat, lease, `fleet-host` |
| [`docs/architecture/README.md`](../../architecture/README.md) | Control-plane layers and tick assignment |
| [`docs/components/sandbox-manager/README.md`](../sandbox-manager/README.md) | Sandbox create / observe / destroy |
| [`run-on-pc-vs-container.md`](run-on-pc-vs-container.md) | Product split: Run on a PC vs Run in a container (how the guest runs) |
| [`docs/components/ui/README.md`](../ui/README.md) | App shell and routes |
