# Self-hosted worker hosts

Paired machines that run agent jobs for a workspace. The control plane is
reachable (IP or domain). Workers only make outbound HTTPS. This is
**create-then-pair** (GitLab 17 / GitHub Actions style), not a shared
registration token and not inbound SSH.

How a guest runs is **Run on a PC** (host process) vs **Run in a
container** (KubePod / in-cluster, or Docker). Same pair, lease, and
`work_claims` path — do not invent a second spawn. See
[`../runners/run-on-pc-vs-container.md`](../runners/run-on-pc-vs-container.md).

Do not replace `fleet-driver`, `fleet-operator`,
`FleetSandbox`, `work_claims`, or session `FLEET_*` tokens. This layer
is a durable **machine** in front of them. The API is not a runner.

Contract for implementers. Exclusive file owners are listed at the bottom.

---

## Requisites (what must be true for this to work)

### Control plane

- `fleet-server` up, Postgres migrated (`009_worker_hosts.sql`).
- Public URL: `API_PUBLIC_BASE` / `ORCHESTRATOR_PUBLIC_URL` (already used for
  `FLEET_ORCHESTRATOR_URL`). Workers heartbeat here. Loopback
  (`127.0.0.1`, `localhost`, `::1`) is fine for a worker on the same box.
  Off-box machines cannot reach loopback. v1 does **not** refuse to assign
  a host when the URL is loopback (local `fleet-dev` would break). The
  Workers pair panel warns; spawn logs `tracing::warn`. Set a reachable
  URL before pairing a remote machine. Follow-up: fail-closed assign when
  the URL is loopback and the host is not this machine.
- Cookie auth for owners who create pair tokens.
- `FLEET_WORKER_DIST_DIR` (optional): directory of prebuilt
  `fleet-host` / `fleet-driver` per platform. If unset,
  install endpoints still serve scripts; binary GET returns 404 with a
  build hint. Dev can set `FLEET_WORKER_BUILD_FROM_SOURCE=1` on the
  **machine** (not the CP) so the platform script runs `cargo build`.
- Identity: the deployer stamps the commit it built into `manifest.json`
  (`FLEET_GIT_SHA`). Worker “current / outdated” compares against
  `[verify]` in `runner-packages.toml`. The author bumps `[versions]`,
  `[verify]`, and the per-OS `-rev`. The deployer fills `[release]`
  hashes from the bins it published. Display is `0.0.3` when rev=0 and
  `0.0.3.1` when rev=1. A line bump resets every OS rev. Not
  `/health.git_sha`. The ship platforms are linux-x86_64, linux-aarch64,
  and windows-x86_64.

### Worker machine

- Outbound TCP to the control plane (HTTPS preferred; HTTP allowed for
  local `http://127.0.0.1:8080`).
- Disk for `work_dir` (default `~/.local/share/fleet-host/work`).
- Clock within a few minutes (pair token TTL is 1 hour).
- **Run on a PC** (host process): `fleet-driver` on PATH (installed
  by the same script). The agent is a normal process on this box.
- **Run in a container** (KubePod / in-cluster, or Docker): same
  `fleet-host` lease loop; the guest is a container. In-cluster
  `fleet-host` creates `FleetSandbox` CRs (`KubePod`). Local `k3d` on
  a paired PC is not a second spawn path.
- A pair token created by a workspace **owner** (1 hour, single use).

### Network / security

- No inbound port on the worker.
- OAuth tokens and integration secrets never leave the control plane.
  Hosts receive `FLEET_*` job env + task files. **Run in a container**
  may also receive a workspace vendor API key (`XAI_API_KEY` /
  `ANTHROPIC_API_KEY`). Never `auth.json` or session files.
- Machine token stored only on the box (`0600`). Hashed (SHA-256 hex) in
  Postgres. Revoke disables the host.
- Pair token hashed at rest; plaintext shown once in the UI.

### Fallback

- If no paired host is `online` with a matching effective run mode,
  leave the task `ready`. The API does not local-spawn.

---

## Settings

Effective value is always the **intersection / min**. The plane can only
restrict. The machine can only restrict further.

A machine is **Run on a PC** or **Run in a container** — never both, never
a silent fallback. Plane `allow_host` / `allow_container` still intersect
with the box’s advertised executor. Create-host defaults may leave
`allow_container=true` as a plane restriction; that flag is not the use
case. See [`../runners/run-on-pc-vs-container.md`](../runners/run-on-pc-vs-container.md).

### Control plane (UI + `PATCH`, stored on `worker_hosts`)

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `name` | string | hostname at pair | Display name |
| `enabled` | bool | true | False = refuse leases; treat as revoked-lite |
| `paused` | bool | false | Online but do not assign new jobs |
| `drain` | bool | false | Like paused; UI copy says “finish current, take no new” |
| `allow_host` | bool | true | Plane permits host-process jobs (**Run on a PC**) |
| `allow_container` | bool | true | Plane permits container guests (**Run in a container**). Not the use-case stamp |
| `max_concurrent` | int 1–256 | 4 | Cap on in-flight sessions on this host |
| `max_job_timeout_s` | int ≥ 60 | 3600 | Cap; job timeout is min(task, this) |
| `labels` | string[] | [] | Informational (gpu, linux, …). Not a scheduler in v1 |

Create-only / actions (not a PATCH blob): revoke, new pair token (pending
hosts or rotate — rotate issues a new pair token only while `pending`;
online hosts use **revoke + re-pair**).

### Worker-local only (`~/.config/fleet-host/config.toml`)

Never accepted from the control plane. Editing requires shell on the box.

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `run_host` | bool | true | This box will exec agents as a host process (**Run on a PC**) |
| `run_container` | bool | false | This box will exec container guests (**Run in a container**: Docker or KubePod) |
| `container_backend` | `k3d` \| `docker` | `k3d` | Local container backend. In-cluster uses KubePod, not this k3d path |
| `work_dir` | path | `~/.local/share/fleet-host/work` | Session dirs |
| `max_concurrent` | int | 8 | Local hard cap (min with CP) |
| `k3d_cluster` | string | `agent-fleet` | Local k3d cluster name if used. Not a second spawn path |
| `k3d_memory` | string | `4g` | passed through when creating a local k3d cluster |
| `k3d_agents` | int | 0 | k3d agents (match `infra/k3d.yaml`) |
| `docker_image` | string | `agent-fleet-api:local` | Image for Docker / KubePod guests |
| `docker_network` | string | `bridge` | Docker network for container jobs. `none` blocks heartbeats to the plane |
| `allow_privileged` | bool | false | Refuse privileged container specs |
| `use_host_git_credentials` | bool | true | Child sees host `~/.git-credentials` / ssh |
| `fleet_driver_bin` | path | `fleet-driver` | Guest in host / docker / kube_pod |
| `log_level` | string | `info` | `error` \| `warn` \| `info` \| `debug` |
| `heartbeat_interval_s` | int | 10 | Host liveness (not the job heartbeat) |
| `poll_interval_s` | int | 2 | Lease poll |
| `http_proxy` | string? | unset | Outbound proxy to reach the CP |
| `extra_env` | map | {} | Injected into children (no `FLEET_WORKER_TOKEN` override) |
| `control_plane_url` | url | from pair | Written at pair time |
| `host_id` | uuid | from pair | Written at pair time |
| `auto_update` | bool | **true** | Mirror of the plane flag. Heartbeat copies `host.auto_update` here so `config.toml` stays in sync. The plane is authoritative. |

`run_host` / `run_container` is the worker-side executor switch
(process vs container). Advertised on every heartbeat as
`advertised_host` / `advertised_container`. The product use case is
separate: **Run on a PC** needs effective host; **Run in a container**
needs effective container (Docker or KubePod). A PC with
`run_container=true` still does not take in-container jobs unless the
host row is stamped that way.

**Effective run modes**

```
host       = cp.allow_host       && local.run_host       && advertised_host
container  = cp.allow_container  && local.run_container  && advertised_container
             && (docker or kube actually healthy)
max_jobs   = min(cp.max_concurrent, local.max_concurrent)
```

If both effective modes are false, the host stays online but never
receives leases (UI shows a warning). Tick assigns by use case
(`run_mode`): **Run on a PC** → `runtime=host`; **Run in a container**
→ `runtime=container`. No silent fallback either way.

---

## Platforms

### Support list (all we should eventually ship binaries for)

| ID | OS | Arch | Notes |
| --- | --- | --- | --- |
| `linux-x86_64` | Linux glibc | x86_64 | glibc ≥ 2.36 (Debian 12 bookworm); Ubuntu 22.04+ / Fedora / RHEL 9+ |
| `linux-aarch64` | Linux glibc | aarch64 | glibc ≥ 2.36 (Debian 12 bookworm cross); Graviton, Pi 4/5 64-bit |
| `linux-armv7` | Linux glibc | armv7l | 32-bit Pi |
| `linux-x86_64-musl` | Linux musl | x86_64 | Alpine |
| `linux-aarch64-musl` | Linux musl | aarch64 | Alpine ARM |
| `linux-armv7-musl` | Linux musl | armv7l | Alpine armhf |
| `linux-riscv64` | Linux glibc | riscv64 | later |
| `linux-ppc64le` | Linux glibc | ppc64le | later |
| `linux-s390x` | Linux glibc | s390x | later |
| `darwin-arm64` | macOS | arm64 | Apple Silicon |
| `darwin-x86_64` | macOS | x86_64 | Intel Mac |
| `windows-x86_64` | Windows | x86_64 | native PowerShell |
| `windows-arm64` | Windows | arm64 | Surface / WoA |
| `freebsd-amd64` | FreeBSD | amd64 | rc.d |
| `freebsd-aarch64` | FreeBSD | aarch64 | later |
| `openbsd-amd64` | OpenBSD | amd64 | later |
| `netbsd-amd64` | NetBSD | amd64 | later |
| `wsl2-*` | WSL2 | (linux-*) | **reuse Linux IDs**; detect `microsoft` in `uname -r` |

### Top 10 (full install scripts in-tree)

1. `linux-x86_64`
2. `linux-aarch64`
3. `linux-armv7`
4. `linux-x86_64-musl`
5. `linux-aarch64-musl`
6. `darwin-arm64`
7. `darwin-x86_64`
8. `windows-x86_64`
9. `windows-arm64`
10. `freebsd-amd64`

Scripts live under `scripts/install-worker/`. Served by fleet-server.

Portable `linux-x86_64` binaries are built on Debian 12 (bookworm, **glibc 2.36**) with `scripts/stage-worker-dist.sh --bookworm` (docker `rust:1.88-bookworm`) and staged to both `linux-x86_64-bookworm/` and `linux-x86_64/` under `$FLEET_WORKER_DIST_DIR`. That glibc is the supported minimum — Debian 12, Ubuntu 22.04+, Fedora, and RHEL 9+ can run them. A native Fedora/host build (glibc 2.39+) will not start on bookworm (`GLIBC_2.39 not found`); do not serve the host debug bin as `linux-x86_64`. Portable `linux-aarch64` is the same bookworm glibc, cross-compiled with `scripts/stage-worker-dist.sh --aarch64` (`rustup target aarch64-unknown-linux-gnu` + `gcc-aarch64-linux-gnu`, isolated `target/docker-aarch64-gnu`). Portable `linux-armv7` is the same bookworm glibc for 32-bit ARM, cross-compiled with `scripts/stage-worker-dist.sh --armv7` (`rustup target armv7-unknown-linux-gnueabihf` + `gcc-arm-linux-gnueabihf`, isolated `target/docker-armv7-gnu`). Alpine guests use `linux-x86_64-musl` (`scripts/stage-worker-dist.sh --musl`), `linux-aarch64-musl` (`scripts/stage-worker-dist.sh --aarch64-musl`, isolated `target/docker-aarch64-musl`), or `linux-armv7-musl` (`scripts/stage-worker-dist.sh --armv7-musl`, isolated `target/docker-armv7-musl`). Windows x86_64 guests use `windows-x86_64` PE binaries (`scripts/stage-worker-dist.sh --windows`, rustup `x86_64-pc-windows-gnu` + mingw-w64 gcc, isolated `target/docker-windows-gnu`). Windows ARM64 guests use `windows-arm64` PE binaries (`scripts/stage-worker-dist.sh --windows-arm64`, rustup `aarch64-pc-windows-gnullvm` + llvm-mingw `aarch64-w64-mingw32-clang`, isolated `target/docker-windows-arm64-gnu`). That stage also copies `libunwind.dll` from the llvm-mingw aarch64 sysroot next to the three `.exe`s; the Windows installer downloads it when the CP serves `GET /install/worker/bin/windows-arm64/libunwind.dll` (404 is skipped so x86_64 gnu is unchanged). `scripts/stage-worker-dist.sh` with no flags still builds natively for the machine you are on.

---

## Install bootstrap

One POSIX script runs on any Unix/WSL/Git-Bash machine:

```
curl -fsSL https://<source>/install-worker.sh | sh
# FreeBSD (stock image has fetch, not curl/wget):
fetch -o - https://<source>/install-worker.sh | sh
```

`<source>` is **A** official download base (`FLEET_OFFICIAL_BASE`, default
empty → prompt) or **B** a control plane the user types
(`https://fleet.example.com` or `http://127.0.0.1:8080`).

Non-interactive (piped, no TTY):

```
FLEET_URL=http://127.0.0.1:8080 curl -fsSL "$FLEET_URL/install-worker.sh" | sh
```

`FLEET_URL` wins. If unset and stdin is a TTY, prompt:

```
Install worker binaries from:
  1) Official  (${FLEET_OFFICIAL_BASE:-not configured — enter a URL})
  2) Control plane (type domain or URL)
```

Then detect `PLATFORM`, download
`$ORIGIN/install/worker/${PLATFORM}.sh` (or `.ps1` on Windows) and exec.

Windows native: `install-worker.ps1` (same A/B prompt).

`FLEET_PAIR_TOKEN` / `FLEET_PAIR_URL` optional: after install, run
`fleet-host pair --url … --token …`.

---

## HTTP API

All JSON. Cookie auth = workspace **owner** (not member) for mutating
host rows. GET list is any member.

### Cookie (owner)

| Method | Path | Body / notes |
| --- | --- | --- |
| `POST` | `/api/workspaces/{id}/worker-hosts` | `{ "name"?: str, "allow_host"?: bool, "allow_container"?: bool, "max_concurrent"?: int }` → `{ host, pair_token, pair_expires_at, install_hint }` |
| `GET` | `/api/workspaces/{id}/worker-hosts` | `{ "hosts": [HostPublic…] }` |
| `GET` | `/api/workspaces/{id}/worker-hosts/{hid}` | HostPublic |
| `PATCH` | `/api/workspaces/{id}/worker-hosts/{hid}` | any CP settings keys |
| `DELETE` | `/api/workspaces/{id}/worker-hosts/{hid}` | revoke (status=`disabled`, tokens invalid) |
| `POST` | `/api/workspaces/{id}/worker-hosts/{hid}/pair-tokens` | only if `pending`; new pair token |

`install_hint` example:

```
curl -fsSL {api}/install-worker.sh | FLEET_URL={api} sh
fleet-host pair --url {api} --token {pair_token}
```

`HostPublic` never includes hashes or raw tokens.

```json
{
  "id": "uuid",
  "workspace_id": "uuid",
  "name": "studio",
  "status": "pending|online|offline|disabled|error",
  "enabled": true,
  "paused": false,
  "drain": false,
  "allow_host": true,
  "allow_container": true,
  "max_concurrent": 4,
  "max_job_timeout_s": 3600,
  "labels": [],
  "advertised_host": true,
  "advertised_container": false,
  "platform": "linux-x86_64",
  "hostname": "box",
  "last_seen_at": null,
  "last_error": null,
  "created_at": "…",
  "paired_at": null,
  "software": null,
  "desired": null,
  "software_status": "unknown",
  "outdated": false,
  "auto_update": true,
  "current_jobs": 0,
  "hardware": null
}
```

`offline` is computed: `status` stored as `online` but `last_seen_at`
older than 90s → serialize as `offline`.

`desired` is the staged dist `manifest.json` for `host.platform`, or
`null` if that platform was not staged. Source of truth is the **served
dist**, not `/health.git_sha`.

`software_status` is `current` | `behind` | `unknown`. `outdated` is
true only when the status is `behind`.

- No software report → `unknown` (not outdated).
- No manifest for `host.platform` → `unknown` (no prebuilt).
- Compare `fleet-host` and `fleet-driver` independently. Version
  (`0.0.1`) is the operator-facing number and is the same on every OS.
  File sha256 is integrity on one OS. Version mismatch, or same version
  with a hash mismatch, is `behind`. Mixed match/mismatch is behind.
- Old hosts that do not report a version still compare by hash.
- If no bin pair exists, fall back to `software.git_sha` vs
  `manifest.git_sha` when both are non-empty.
- All compared bins (or both git shas) match → `current`.

### Pair / machine (no cookie)

| Method | Path | Auth | Body |
| --- | --- | --- | --- |
| `POST` | `/api/worker-hosts/pair` | pair token once | `{ "token", "hostname", "platform", "advertised_host", "advertised_container" }` → `{ "host_id", "machine_token", "workspace_id", "control_plane_url", "cp_settings" }` |
| `POST` | `/api/worker-hosts/me/heartbeat` | `Authorization: Bearer flm_…` | `{ "advertised_host", "advertised_container", "platform"?, "hostname"?, "current_jobs": n, "error"?: str, "software"?: { "git_sha", "bins", "updating" }, "hardware"?: { … } }` → `{ "host": HostPublic, "effective": { "host", "container", "max_concurrent" }, "desired": { "git_sha", "platform", "bins" } \| null }` |

`hardware` is a one-shot jsonb snapshot (not a series). Keep existing keys; new keys are optional and omitted when the box cannot read them. Never fail the heartbeat.

| Key | Type | Notes |
| --- | --- | --- |
| `cpu_cores` | u32 | logical CPUs |
| `cpu_model` | string? | e.g. `AMD Ryzen 7 5700G with Radeon Graphics` |
| `cpu_used_pct` | u8? | 0–100, delta across heartbeats; first sample may omit |
| `mem_total_bytes` | u64 | |
| `mem_used_pct` | u8 | |
| `mem_speed_mts` | u32? | DIMM MT/s; often unavailable without readable DMI |
| `mem_type` | string? | `DDR4` / `DDR5` when DMI/WMI has it |
| `gpus` | `[{ "name", "vram_bytes"?, "used_pct"? }]?` | nvidia-smi / DXGI / lspci / sysfs; iGPU counts |
| `gpu_used_pct` | u8? | 0–100, max of readable GPUs (`nvidia-smi` util or amdgpu `gpu_busy_percent`) |
| `os` | string | |

| Method | Path | Auth | Body |
| --- | --- | --- | --- |
| `POST` | `/api/worker-hosts/me/lease` | machine bearer | `{ }` → `{ "claim": null }` or `{ "claim": { "id", "lease_token", "payload": SpawnPayload } }` |
| `POST` | `/api/worker-hosts/me/lease/{claim_id}/complete` | machine bearer | `{ "lease_token", "ok": bool, "error"?: str }` |

`SpawnPayload` (also the `work_claims.payload` JSON):

```json
{
  "kind": "spawn_session",
  "session_id": "uuid",
  "project_id": "uuid",
  "run_id": "uuid",
  "board_task_id": "uuid",
  "plan_task_id": "string",
  "worker_profile": "string",
  "worker_token": "uuid-string",
  "orchestrator_url": "http://…",
  "host_id": "uuid",
  "runtime": "host|container",
  "task_title": "",
  "task_spec": "",
  "timeout_s": 3600,
  "repo_url": null,
  "git_ref": null,
  "env": { "FLEET_SESSION_ID": "…", "FLEET_WORKER_TOKEN": "…" }
}
```

Use existing `work_claims` (`kind = spawn_session`,
`lease_owner = host_id`). `claim_next(workspace_id, host_id, lease_for=2min)`.

### Install (public GET, text)

| Path | File |
| --- | --- |
| `/install-worker.sh` | `scripts/install-worker/install-worker.sh` |
| `/install-worker.ps1` | `scripts/install-worker/install-worker.ps1` |
| `/install/worker/{name}` | `scripts/install-worker/{name}` (`..` rejected) |
| `/install/worker/bin/{platform}/{bin}` | `$FLEET_WORKER_DIST_DIR/{platform}/{bin}` or 404 |
| `/install/worker/bin/{platform}/manifest.json` | staged package identity (`git_sha`, `platform`, `bins.{name}.{sha256,size,version}`). 404 if that platform is not staged. Windows keys are `fleet-host.exe` etc. `version` is the same on every OS. |

---

## Schema (`crates/fleet-db/migrations/009_worker_hosts.sql`)

```sql
CREATE TABLE worker_hosts (
    id                   UUID PRIMARY KEY,
    workspace_id         UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    name                 TEXT NOT NULL,
    status               TEXT NOT NULL DEFAULT 'pending'
                         CHECK (status IN ('pending','online','disabled','error')),
    enabled              BOOLEAN NOT NULL DEFAULT TRUE,
    paused               BOOLEAN NOT NULL DEFAULT FALSE,
    drain                BOOLEAN NOT NULL DEFAULT FALSE,
    allow_host           BOOLEAN NOT NULL DEFAULT TRUE,
    allow_container      BOOLEAN NOT NULL DEFAULT TRUE,
    max_concurrent       INT NOT NULL DEFAULT 4
                         CHECK (max_concurrent >= 1 AND max_concurrent <= 256),
    max_job_timeout_s    INT NOT NULL DEFAULT 3600 CHECK (max_job_timeout_s >= 60),
    labels               TEXT[] NOT NULL DEFAULT '{}',
    machine_token_hash   TEXT,
    advertised_host      BOOLEAN NOT NULL DEFAULT FALSE,
    advertised_container BOOLEAN NOT NULL DEFAULT FALSE,
    platform             TEXT,
    hostname             TEXT,
    last_seen_at         TIMESTAMPTZ,
    last_error           TEXT,
    created_by           UUID REFERENCES users (id) ON DELETE RESTRICT,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    paired_at            TIMESTAMPTZ,
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX worker_hosts_ws_idx ON worker_hosts (workspace_id, status);

CREATE TABLE worker_host_pair_tokens (
    id           UUID PRIMARY KEY,
    host_id      UUID NOT NULL REFERENCES worker_hosts (id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspaces (id) ON DELETE RESTRICT,
    token_hash   TEXT NOT NULL UNIQUE,
    expires_at   TIMESTAMPTZ NOT NULL,
    used_at      TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Token helpers (same idea as orch `hash_worker_token`): SHA-256 hex,
constant-time verify. Prefixes: pair `flp_`, machine `flm_`. Generate
32 random bytes, hex-encode after the prefix.

---

## Tick (orchestrator)

In `schedule_and_spawn`, after `begin_spawn_assignment` (session + job
token **unchanged**):

1. List worker_hosts for `project_id` (= workspace_id) that are
   `enabled`, not `paused`, not `drain`, `status` stored `online`,
   `last_seen_at` within 90s, `machine_token_hash` set.
2. Compute effective modes; skip hosts that do not match the job’s
   use case. **Run on a PC** needs effective host; **Run in a container**
   needs effective container (KubePod in-cluster, or Docker). Same
   `work_claims` path. Do not enqueue the other runtime. No silent
   fallback.
3. Count in-flight sessions for that host
   (`runtime_backend = 'remote'` and `runtime_handle = 'host:{id}'`
   and status in `spawning`/`healthy`). Skip if ≥ effective max.
4. If a host is chosen: enqueue `work_claims` `spawn_session` with the
   payload above; `complete_spawn_success` with handle `host:{id}` and
   backend `remote`. Do **not** `ProcessRuntime::spawn` on the API box.
5. If none: leave the task `ready`. The control plane is not a host and
   is not listed on `GET …/worker-hosts`. Production does not local-spawn
   on the API box.
6. **Exception:** if no assignable host but a paired host in this workspace
   is `software_updating` and `last_seen` within ~2 minutes, **do not**
   assign elsewhere. Leave the task `ready`. Offline without that flag
   still leaves the task ready.

`fleet-orchestrator` may depend on `fleet-db` for `work_claims::enqueue`
and `worker_hosts` list. Do not invent a second claims table.

Stuck/kill: session still uses job heartbeats from `fleet-driver`.
If the host goes offline, existing stale-heartbeat logic applies.

---

## `fleet-host` crate

New workspace member `crates/fleet-host`. Binary `fleet-host`.

```
fleet-host pair --url URL --token TOKEN
fleet-host run
fleet-host update
fleet-host status
fleet-host unpair
```

`pair` writes `~/.config/fleet-host/config.toml` (create dirs `0700`)
and `~/.config/fleet-host/machine.token` (`0600`).

`run` loop: heartbeat every `heartbeat_interval_s`; `POST …/lease`;
on claim, spawn child:

- `runtime=host` (**Run on a PC**): `fleet_driver_bin` (ACP;
  `FLEET_HARNESS` mock|grok), cwd = `{work_dir}/{session_id}`, env =
  payload.env + extra_env. Same HTTP contract as `ProcessRuntime`.
- `runtime=container` (**Run in a container**): same lease loop.
  In-cluster / kubeconfig → `KubePod` (`FleetSandbox` CR). Docker →
  `docker run` with `docker_image` and the same `FLEET_*` env.
  Never fall back KubePod → Exec. Local k3d on a paired PC is not a
  second spawn path.
- Refuse if the host’s use case or effective mode does not match
  the payload runtime.

Job child still POSTs `/v1/.../heartbeat|status` with the **job** token.

---

## Apply (software update)

Manual (`fleet-host update`) and auto share one path. **Never set CP
`paused` / `drain` for an update** — those flags drop the host from
`list_assignable` and the task would stay ready with no runner.

Heartbeat response (host client parses even if an older CP omits it):

```
{ "host": { "auto_update": true, "paused", "drain", … },
  "effective": { … },
  "desired": {
    "git_sha": "…",
    "platform": "windows-x86_64",
    "bins": {
      "fleet-host.exe": { "sha256": "…", "size": N, "version": "0.0.1" },
      "fleet-driver.exe": { "sha256": "…", "size": N, "version": "0.0.1" }
    }
  } | null }
```

Unix bin names omit `.exe`. Platform IDs match `detect_platform()` /
install (`linux-x86_64`, `windows-x86_64`, …).

1. Effective auto = plane `host.auto_update` (default **true**). The host
   writes that value into `config.toml` on heartbeat so the file stays a
   mirror; a stale local `false` does not restrict. Auto-off still applies
   when the operator runs `fleet-host update` or the CP sets
   `update_requested_at`.
2. After heartbeat: sync `config.toml`, then if auto is on and `desired`
   differs, spawn apply (do not block the heartbeat). One attempt per
   desired git sha.
3. No `desired` / no prebuilt / 404 → stay quiet; do not error-loop.
4. Hash local bins; all match → no download.
5. Download **changed** bins to `{bin}.new` from the paired
   `control_plane_url` (`/install/worker/bin/{platform}/{bin}` +
   `.sha256`) **while still claiming**. SHA-256 required. Windows PE
   machine-type check (`0x8664` vs `0xAA64`). All downloads verify
   before any rename; failure deletes `.new` and keeps the running set.
6. **Children only** (`fleet-host` hash unchanged): `rename` worker/driver.
   Keep leasing. In-flight jobs finish on the old inode.
   `software.updating` stays false.
7. **Host binary changed:** heartbeat `software.updating=true`, **stop
   claiming** new leases (do not PATCH drain/paused). Wait for in-flight
   to exit. Claims already in `work_claims` for this `host_id` wait.
   - Unix: `mv` `.new` into place; `exec` (or `systemctl --user restart`).
   - Windows: cannot overwrite a running PE. Write `update.cmd` next to
     the exe, spawn it detached, **exit**. Script waits for
     `fleet-host.exe` to leave, `move /Y` `.new` → `.exe` for changed
     bins, starts `fleet-host.exe run`.
8. Do **not** unpair. `machine.token` + `config.toml` stay.

---

## UI

New route `/{workspace_id}/workers` (sidebar: Workspace → Workers).

- List workers: name, status chip, last seen, hostname/platform,
  software line (`up to date` / `outdated` / `unknown`) — no hex on the
  card (git bake vs file hash are not the same id).
  `updating` replaces outdated while a swap is in progress. Outdated is
  amber. Desired bits come from the staged dist, not the API compile sha.
- **Add worker**: POST (name optional; hostname on pair), show pair token **once** + install commands
  (copy). Token is not shown again.
- Everyday: Stop taking new jobs / Take jobs again, Auto-update (on by default), Update now if auto is off and the box is outdated. Rename by clicking the name. Each box has a durable `machine_id` (`~/.config/fleet-host/machine.id`, or the first pair’s row id if the binary is too old to send one). Re-pair of the same `machine_id` or the same hostname+platform reclaims that row. Removed clones of a live box are hidden.
- Remove worker (revoke). Card badge is the use case (**On a PC** /
  **In a container**). `allow_host` / `allow_container` stay advanced
  technical, not the product name.

Human-test + `record_demo.py` scene `workers` in the same change.

---

## Tests

Existing SUT → path (partial coverage). **Required** layers for every
behavior: [`../runners/testing-requirements.md`](../runners/testing-requirements.md)
(unit / browser e2e / real-life VM install on every OS). That file is
the requirement list, not the implementation.

| SUT | Where |
| --- | --- |
| Token hash, effective modes | `fleet-db` or `fleet-host` `#[cfg(test)]` |
| Repos: create, pair consume-once, revoke, heartbeat | `crates/fleet-db/tests/worker_hosts_repos.rs` |
| HTTP pair / lease / owner-only | `crates/fleet-server/tests/worker_hosts_http.rs` |
| Tick prefers remote when host online, else local | orch unit/integration |
| `fleet-host` pair parse + config write | `crates/fleet-host` |
| Bootstrap detector | `sandbox-lab/tests/test_install_worker.sh` or a small pytest if you can exec the detector dry-run (`FLEET_DETECT_ONLY=1`) |
| UI chrome | Playwright scene `workers` |

Fail closed: missing `DATABASE_URL` panics. No `SKIP_*`.

---

## Exclusive file owners (do not edit others' files)

| Owner | Paths |
| --- | --- |
| **db** | `crates/fleet-db/migrations/009_worker_hosts.sql`, `crates/fleet-db/migrations/010_worker_host_software.sql`, `crates/fleet-db/migrations/011_worker_host_hardware.sql`, `crates/fleet-db/src/models/worker_host.rs`, `crates/fleet-db/src/repos/worker_hosts.rs`, `crates/fleet-db/src/models/mod.rs` (export only), `crates/fleet-db/src/repos/mod.rs` (mod only), `crates/fleet-db/tests/worker_hosts_repos.rs` |
| **api** | `crates/fleet-server/src/worker_hosts.rs`, `crates/fleet-server/src/install_routes.rs`, `crates/fleet-server/src/lib.rs` (merge + `/api` index links only), `crates/fleet-server/src/main.rs` (only if needed), `crates/fleet-server/tests/worker_hosts_http.rs`, `.env.example` (new keys only) |
| **orch** | `crates/fleet-orchestrator/Cargo.toml` (add `fleet-db`), `crates/fleet-orchestrator/src/orch/spawn.rs`, `crates/fleet-orchestrator/src/sandbox/remote.rs` (new), `crates/fleet-orchestrator/src/sandbox/mod.rs` (`remote` is the production backend), `crates/fleet-orchestrator/src/store/host_pool.rs` (new), orch tests you add as new files |
| **host** | `crates/fleet-host/**`, root `Cargo.toml` `members` + `workspace.dependencies.fleet-host` only |
| **ui** | `crates/fleet-ui/src/pages/workers/**`, `pages/mod.rs`, `main.rs` route, `components/sidebar.rs` link |
| **docs** | `docs/human-testing.md`, `scripts/human-test.sh` (one step), `tests/browser/record_demo.py` scene `workers`, `README.md` (workers row), `docs/architecture/README.md` one paragraph, `.env.example` only if api left a placeholder |

**install scripts** are already in `scripts/install-worker/` — api serves them;
do not rewrite unless broken.

Do not edit `crates/fleet-server/tests/http_api.rs` or
`crates/fleet-db/tests/repos_integration.rs`.
