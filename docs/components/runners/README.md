# Runners

One product surface for **where work runs**. Five execution kinds, one
assignment path, two isolation modes.

Self-hosted pairing, install, heartbeat, and lease stay in
[`docs/components/worker-host/README.md`](../worker-host/README.md).
This folder is the umbrella: kinds, isolation, nav, and routes.

---

## Five kinds

| Kind | Who owns the machine | Isolation | Status |
| --- | --- | --- | --- |
| Self-hosted native | Customer PC / server / VPS | **Native** (not a sandbox) | **MR 1** — paired `fleet-host` |
| Self-hosted sandbox | Same machine, k8s/containers on it | Sandboxed | **MR 2** toggle placeholder; executor is not v1 |
| Public cloud | Customer AWS / GCP / Azure keys | Sandboxed | **MR 3** — placeholder |
| Fleet Cloud | Our paid pool | Sandboxed | **MR 3** — placeholder |
| Partners | Other people’s unused capacity | Sandboxed | **MR 3** — placeholder + [design](partners.md) |

Tick already prefers an assignable remote host, else local
`SANDBOX_BACKEND`. Later MRs extend *who* can be assignable. Do not
invent a second spawn path.

---

## Isolation is not a nav item

Each self-hosted runner has two checkboxes: **Native** and **Sandboxed**.
Both, either, or neither. Neither is the same as stop taking new jobs.

- **Native** — jobs run as processes on that OS. This is what v1 assigns.
- **Sandboxed** — placeholder. Shown on the card so the option is not
  missing; checking it does not run container/k8s jobs yet.

Sidebar after MR 3: **Runners** accordion (same rank as Monitoring and
Agents). Children: **Self-hosted** (live), **Cloud** and **Partners**
(placeholders this MR). Workspace keeps Integrations + Settings.

---

## Naming

Do **not** rename `fleet-host`, `fleet-worker`, or the `worker_hosts`
table. API paths stay `/api/workspaces/{id}/worker-hosts` and
`/api/worker-hosts/…`.

**Runners** is the product word (a machine that executes someone else’s
job). **Workers** as a nav section collides with the per-job
`fleet-worker` process.

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
| [`docs/components/runners/partners.md`](partners.md) | Partners credit, trust, kill-switch (design) |
| [`docs/components/worker-host/README.md`](../worker-host/README.md) | Pair, install, heartbeat, lease, `fleet-host` |
| [`docs/architecture/README.md`](../../architecture/README.md) | Control-plane layers and tick fallback |
| [`docs/components/sandbox-manager/README.md`](../sandbox-manager/README.md) | Sandbox create / observe / destroy |
| [`docs/components/ui/README.md`](../ui/README.md) | App shell and routes |
