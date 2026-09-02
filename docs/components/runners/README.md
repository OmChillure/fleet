# Runners

One product surface for **where work runs**. Five execution kinds, one
assignment path, two isolation modes.

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
| Self-hosted native | Customer PC / server / VPS | **Native** (not a sandbox) | **MR 1** — paired `fleet-host` |
| Self-hosted sandbox | Same machine, k8s/containers on it | Sandboxed | **MR 2** toggle placeholder; executor is not v1 |
| Public cloud | Customer AWS / GCP / Azure keys | Sandboxed | **MR 3** — placeholder |
| Fleet Cloud | Our paid pool | Sandboxed | **MR 3** — placeholder |
| Partners | Other people’s unused capacity | Sandboxed | **MR 3** — placeholder + [design](partners.md) |

Tick assigns to a paired `fleet-host`. The API process is not a runner
and does not appear on Self-hosted. No host → the task stays ready.
Same-machine install is **Add worker** then pair. Do not invent a
second spawn path.

---

## Isolation is SANDBOX_BACKEND (once)

Do not add Native / Sandboxed checkboxes on runner cards. How a job is
isolated is `SANDBOX_BACKEND` (`stub` / `process` / `local` / `docker`)
on the control plane. Paired `fleet-host` boxes run a process on that
machine. Pause / “Stop taking new jobs” is not isolation.

Sidebar: **Runners** accordion (same rank as Monitoring and Agents).
Children: **Self-hosted** (live), **Cloud** and **Partners**
(placeholders). Workspace keeps Integrations + Settings.

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
| `/{workspace_id}/runners/cloud` | Cloud placeholders |
| `/{workspace_id}/runners/partners` | Partners + trust copy ([design](partners.md)) |

---

## Related

| Doc | Why |
| --- | --- |
| [`testing-requirements.md`](testing-requirements.md) | Every runner behavior needs unit, e2e, and real-life VM/OS tests |
| [`testing-procedures.md`](testing-procedures.md) | How to run those three layers |
| [`docs/components/runners/partners.md`](partners.md) | Partners credit, trust, kill-switch (design) |
| [`docs/components/worker-host/README.md`](../worker-host/README.md) | Pair, install, heartbeat, lease, `fleet-host` |
| [`docs/architecture/README.md`](../../architecture/README.md) | Control-plane layers and tick fallback |
| [`docs/components/sandbox-manager/README.md`](../sandbox-manager/README.md) | Sandbox create / observe / destroy |
| [`docs/components/ui/README.md`](../ui/README.md) | App shell and routes |
