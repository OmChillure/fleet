# Runner testing requirements

Required coverage for every runner behavior on this branch. How to run
the three layers: [`testing-procedures.md`](./testing-procedures.md).
A behavior is not done until the three layers that apply have tests.

How to *run* tests: [`docs/testing.md`](../../testing.md).
Where a test *lives*: [`docs/testing-standardization.md`](../../testing-standardization.md).
Product surface: [`README.md`](./README.md), [`worker-host`](../worker-host/README.md).

Do not invent a second framework. Rust behavior stays in Rust. Dioxus
chrome stays in Playwright. Install / VM / OS matrix stays in lab
Python next to the other guest tests. Fail closed.

---

## Three layers

Every row below needs all three unless the cell says **n/a**.

| Layer | What it is | Where |
| --- | --- | --- |
| **1. Unit / regular** | Pure logic + API/repo/binary contracts. No browser. Postgres when the SUT is HTTP or SQL. | `#[cfg(test)]` next to the code; `crates/<pkg>/tests/*.rs` (`require_database_url()`). `fleet-ui` helpers that can run on the host stay next to the code or move to a host crate — do not leave them unrunnable. |
| **2. e2e** | Browser against **this tree** (`/health.git_sha` **and** UI `/fleet-git-sha.txt` == `scripts/tree-id.sh`). Click, type, navigate. Same change also adds a `human-test.sh` step and a `record_demo.py` scene when the surface is user-visible. | `tests/browser/test_app_e2e.py`; `scripts/human-test.sh`; `tests/browser/record_demo.py` |
| **3. Real-life e2e** | Real guests. Spawn a VM (or equivalent) per supported OS/arch, install the runner from the pair command this plane serves, heartbeat, take a job, apply settings, update, go offline/online, reclaim `machine.id`. Not a screenshot of the pair panel. | Lab / VM matrix (same install-script / guest harness family as existing `.tmp/vms` / sandbox-lab OS tests). Enable with a repo-root `.env` flag. Missing guests when the flag is on is a **fail**. |

### Real-life matrix (required guests)

Every staged platform this plane serves must have a real-life path:

`linux-x86_64`, `linux-x86_64-musl`, `linux-x86_64-bookworm`,
`linux-aarch64`, `linux-aarch64-musl`, `linux-armv7`, `linux-armv7-musl`,
`freebsd-amd64`, `darwin-x86_64`, `darwin-arm64`,
`windows-x86_64`, `windows-arm64`.

A platform listed under **Serving this build** / **Outdated packages**
without a guest test is a hole.

**Live skipped for now:** `linux-aarch64`, `linux-aarch64-musl`,
`linux-armv7`, `linux-armv7-musl`, `freebsd-amd64`, `darwin-x86_64`,
`darwin-arm64`, `windows-arm64`. Catalog still lists the ids.
Re-enable in `test_runner_guest_e2e.py` `LIVE_SKIPPED`.

### What real-life must exercise (when the behavior applies)

Install from the pair command (Unix script and Windows PowerShell),
daemon start, heartbeat / last-seen / offline after 90s, lease + one
native job, settings apply + ack (`pending apply` then clear),
auto-update and Update now, software current vs behind vs updating,
`machine.id` survive remove + re-pair, unique id / reclaim, GPU/iGPU
inventory, plane runner taking / not taking jobs vs local spawn.

---

## Rule

- Adding or changing a behavior in the catalog **requires** the three
  layers in the **same change** (or an explicit **n/a** in this file
  with why).
- Placeholders (Cloud, Partners, Sandboxed executor) still need layers
  1–2 so they cannot silently become assignable. Layer 3 is **n/a**
  until that kind actually runs a job.
- Do not mark a layer done because a neighboring test exists. The row
  is the unit of coverage.

---

## Catalog

### Navigation and routes

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Runners accordion + Self-hosted / Cloud / Partners links | Route table / nav labels | Open each child; heading matches | n/a |
| `/{ws}/runners` → Self-hosted | Redirect helper | Land on Self-hosted | n/a |
| `/{ws}/workers` → Self-hosted | Redirect helper | Land on Self-hosted | n/a |

### Add worker and pair

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Add worker creates a pending host + one-time pair token | HTTP create; token hashed; plaintext once | Click Add worker; panel shows token once; gone after Done + reload | Pair from a guest using that token |
| Unix install command (Linux, macOS, FreeBSD) | Script contract; `FLEET_PAIR_TOKEN` present | Command in the panel contains token + `install-worker.sh` | Guest runs the command; bins install; daemon starts |
| Windows PowerShell install | Same for `.ps1`; ARM64 vs x64 from the machine | Command in the panel contains token + `install-worker.ps1` | Windows x64 and ARM64 guests |
| Copy (Unix, Windows, token) | Clipboard helper | Copy buttons work (incl. HTTP LAN fallback) | n/a (browser) |
| Token expires (1 hour), single use | Repo consume-once + expiry | n/a beyond “shown once” | Expired / reused token fails on the guest |
| Loopback URL warning | Hint detector | Warning when command is loopback | Off-box guest cannot pair to loopback; LAN/public URL can |
| Done keeps the pending worker, closes the panel | — | Done hides panel; card still pending | n/a |
| Cancel discards the unused worker, closes the panel | Revoke never-paired stays hidden | Cancel; leftover not in the list | n/a |
| Install progress + SHA reuse | Sidecar SHA; reuse only on match | n/a | Second install skips matching bins, refetches mismatch |
| Daemon starts after pair on every OS | Host start path | n/a | After install, host is online without a second command |

### List, identity, status

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Poll list ~10s | — | Card updates after pair / heartbeat | Guest heartbeat moves last-seen + online |
| Control plane always first, then status, then name | Sort helper | First card is Control plane | n/a |
| Rename (click name, Enter / Escape) | PATCH name | Rename a card | n/a |
| Pills: online, offline, pending, removed, error | `public_status`; 90s offline | Pending card; offline after guest stop | Stop/start guest: offline after 90s, online on heartbeat |
| Extra pills: pending apply, outdated | `extra_status_pills` | After Apply, pending apply until ack | Guest ack clears pending apply; behind package shows outdated |
| last seen relative | `relative_seen` | Shows “just now” / “never” | Heartbeat updates last seen |
| Offline does **not** repeat a heartbeat sentence | `not_taking_reason` | Body has no “heartbeating” | n/a |
| Stable 8-hex id from `machine_id` (else host id) | `worker_id_label`; reclaim | Regex `id [0-9a-f]{8}` | Same id after remove + re-pair on the same guest |
| Hostname · platform | Heartbeat fields | Shown after pair | Guest report matches the OS |
| Specs: CPU, cores, RAM, GPUs (incl. Intel iGPU), load % | Formatters; GPU normalize | Specs on a live card | Linux / Windows / macOS / FreeBSD inventory |
| Jobs `n / cap` | Cap = min(plane, advertised) | `jobs 0 / 4` on a new card | Running a job increments current |
| Software line on **paired boxes only** | `software_line` / `software_flag` vs staged dist, never file hash as bake id | Software line after pair; not on plane | current / behind / updating / old package / unknown on a real box |
| Not-taking reasons (pair / removed / error / hold) | `not_taking_reason` | Waiting to pair on new card | Error from guest surfaces |
| Show / hide removed; hide clones + never-paired | `in_removed_list` | Show removed after revoke of a paired box | Re-pair hides the old clone |

### Isolation

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Isolation label + Native + Sandboxed per card | Component + PATCH `allow_host` / `allow_container` | Both checkboxes on a paired card | Native on → guest takes a host job |
| Both off = not taking new jobs | `holding_new_jobs` | Stop taking jobs unchecks both | Guest gets no new lease |
| Sandboxed is a placeholder (not assignable, no k8s job) | `list_assignable` excludes sandboxed; no `runtime=container` | Hint “not implemented yet”; no container enqueue | Must **not** start a container/k3d job |
| Plane: Native always off, disabled, greyed out | UI lock; plane `taking_native_jobs` is still `allow_host` (Behavior) | Plane Native unchecked + not clickable | n/a |
| Plane Sandboxed still toggles (still placeholder) | PATCH plane `allow_container` | Toggle Sandboxed on the plane card | n/a |

### Behavior (jobs, update, cap)

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Behavior label separate from Isolation | — | Isolation then Behavior on the card | n/a |
| Stop taking new jobs / Take jobs again | Hold copy; PATCH; does not set pause for updates | Button visible; draft then Apply | After apply+ack, guest stops / resumes leases; running job finishes |
| Auto-update on (paired only) | Heartbeat desired + host update path | Checkbox on paired card; absent on plane | Guest pulls served package when behind and idle |
| Auto-update off + Update now | `request_update` | Button when behind; becomes Update requested | Guest applies once on next heartbeat |
| Mid-update: hold ready, do not local-spawn | `hold_ready_for_host_update` | n/a | Kill guest mid-swap; task stays ready, no local fallback |
| Max jobs at once (1–256, min with advertised) | Clamp + assignable skip at cap | Input on the card | Second job waits when cap is 1 |

### Apply / ack (paired)

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Edits stay local until Apply changes | Draft vs host | Toggle Auto-update → Apply appears | n/a |
| Apply sends PATCH, bumps `settings_gen` | Repo bump | Toast; pending apply | Guest heartbeat acks gen; pill clears |
| Widgets lock while pending | `settings_pending` | Inputs disabled while pending | n/a |
| Old host that never acks stays pending apply | gen ≠ ack | — | Old `fleet-host` binary never clears the pill |
| Remove worker (confirm / cancel) | Revoke → disabled | Confirm removes; Cancel aborts | Revoked guest cannot lease; re-pair required |

### Control plane runner

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Always first; `kind=control_plane`; stable per-ws id | `control_plane_runner_id`; list prepend | First card name Control plane, “this process” | n/a |
| Not a `worker_hosts` row (no pair / lease / heartbeat) | GET/PATCH plane id; DELETE 400; no pair-token | No Remove; no pair panel on the plane | Must **not** appear in `list_assignable` |
| Settings persist (`workspace_plane_runners`) | upsert + default-on | Apply on plane is immediate (no pending apply) | n/a |
| No software line on the plane card | `as_public` software = none | Plane card has no `software …` | n/a |
| Stop taking jobs on plane → no local `SANDBOX_BACKEND` | `taking_native_jobs`; spawn skip | Stop + Apply on plane | With no remote host, a ready task stays ready |
| Plane taking jobs + no remote → local spawn | Default allow_host true | — | Ready task runs locally when plane is on |
| Local job count vs max / jobs on the plane card | **required when implemented** (TODO today) | same | same |

### Package banner (header)

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Current runner build = served runner set, 12-char short | `package_report`; ignore plane SHA | “Current runner build” + 12 hex; no `Served packages` mash | Staged dirty vs clean SHA shortens like the card |
| Serving this build: always shown (blank if none) | empty `serving` still rendered | Line present even when all outdated | After restage, platforms move to Serving |
| Outdated packages: older / no build id only | Mixed SHA / no bake id | Amber line when mixed | Restage one platform; only that line is current |
| Plane SHA must not mark packages outdated | dirty plane ≠ clean package | No false “all outdated” after API rebuild | n/a |

### Assignment / tick

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| One picker: `select_assignable_runner` | Wrapper + native-only filter | n/a (orch) | Job lands on an online native guest |
| Prefer remote online native under cap | `list_assignable` + in-flight | n/a | Two guests: job goes to a free one |
| Else local only if plane taking jobs | spawn gate | n/a | Plane off + guests down → no local spawn |
| Cloud / Partners / public-cloud never chosen | Picker ignores them | Placeholder pages do not enqueue | n/a |
| No container / k3d enqueue in v1 | Spawn payload host-only | n/a | Sandboxed-only host gets no job |

### Software staging and host update

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Heartbeat reports bake id + bins + hardware | Host JSON; repo persist | Card software + specs | Guest report matches staged / hardware |
| current / behind / unknown vs staged manifest | `compute_software_status` | Software flag on the card | Install old package → behind; restage+update → current |
| Never treat a file hash as the bake id | `software_build_id` | Card does not show bin SHA as version | n/a |
| Stage per-platform manifests | Stage script / install routes | Header lists served platforms | Guest for each platform installs that package |
| `machine.id` survives update + reinstall | Reclaim on pair | Same id after re-pair | Guest update/reinstall keeps id |

### Cloud (placeholder)

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| AWS / GCP / Azure / Fleet Cloud cards, Coming soon | Page render helpers | Visit `/runners/cloud`; four cards; no enqueue | **n/a** until a cloud kind runs a job |
| Cannot become assignable by accident | Picker + HTTP must not create cloud hosts | No spawn/lend controls enabled | n/a |

### Partners (placeholder)

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Coming soon + trust copy | Copy constants | Visit `/runners/partners` | **n/a** until lend/borrow runs a job |
| Disclaimer ack is a timestamp only | Default off; first-write-wins; ack ≠ assignable | Ack persists; no lend enablement | n/a |
| Ack does not open the pool or create credit | Repo + HTTP | No balance / lend button enabled | n/a |

### Operator identity (blocks false-green e2e)

| Behavior | 1. Unit / regular | 2. e2e | 3. Real-life |
| --- | --- | --- | --- |
| Live stack must be this tree | `stack_identity.py` contract | Browser e2e fails on foreign `:8080`/`:3030` | Guest install must pull **this** staged dist, not an old SHA |

---

## Closed / not this file

Implementing any row is a later change. Do not treat this catalog as
green. When a layer is written, point at the test in the crate README
or in [`testing-standardization.md`](../../testing-standardization.md)
— do not delete the row.
