# fleet-host

Paired machine agent. Outbound HTTPS only. Pairs with a control plane, then
heartbeats and leases `spawn_session` jobs.

Config lives on the box and is **never** accepted from the control plane:

```
~/.config/fleet-host/config.toml     # 0700 directory
~/.config/fleet-host/machine.id      # durable box id; survives unpair
~/.config/fleet-host/machine.token   # 0600, written by `pair`
```

Override the config directory with `FLEET_HOST_CONFIG_DIR` (tests / unusual
homes). Work dirs default to `~/.local/share/fleet-host/work`.

## Pair

A workspace **owner** creates a one-hour pair token in the UI, then on this
machine:

```bash
fleet-host pair --url https://fleet.example.com --token flp_…
# local / kind:
fleet-host pair --url http://127.0.0.1:8080 --token flp_…
```

That writes `host_id` + `control_plane_url` into `config.toml` and the
machine token into `machine.token`. Local run-mode keys are left as-is if
the file already exists (so `unpair` + re-pair keeps your settings).

Compose / kind start `fleet-host run` with `FLEET_CONTROL_PLANE_URL` and
`FLEET_BOOTSTRAP_SECRET`. The process calls
`POST /api/worker-hosts/bootstrap` until a workspace exists, then
heartbeats. `FLEET_HOST_RUN_HOST` / `FLEET_HOST_RUN_CONTAINER` overlay
`config.toml`. In-cluster sets `FLEET_HOST_RUN_CONTAINER=true` (KubePod).

```bash
fleet-host status    # host_id, url, run_host, run_container, token present
fleet-host run       # heartbeat + lease loop (systemd / tmux / foreground)
fleet-host update    # apply CP-served bins (same path as auto-update)
fleet-host unpair    # delete machine.token only; config stays
```

## Agent runtime vs workspace location

Two axes, never one isolation enum for this box.

| Axis | Meaning | Wire today |
| --- | --- | --- |
| **agent_runtime** | Where the agent runs: host process (TUI) vs container/VM | `run_mode` `on_pc` \| `in_container` (JSON alias `agent_runtime`) |
| **workspace_location** | Where the repo lives: host vs container/VM | workspace `min_isolation` `process` \| `container` \| `vm` |

Host `isolation` `native`/`sandboxed` is derived from agent_runtime
(`on_pc`→`native`, `in_container`→`sandboxed`). k8s is placement (a
pod is still a container). Kind/container is assignable.

| agent_runtime | Executor | This box will… |
| --- | --- | --- |
| `on_pc` (`runtime=host`) | exec | `fleet-driver` on the host (`FLEET_HARNESS=mock\|grok`) |
| `in_container` | docker | `docker run` (`--network` default `none`). Same `FLEET_*` as host. Missing docker is an error — never Host. |
| `in_container` | kube_pod | In-cluster (or `KUBECONFIG`) creates a `FleetSandbox` CR. Guest is `fleet-driver` (ACP). |
| `in_container` (vm guest) | firecracker / qemu | Allowed only when a hypervisor is on `PATH`. `run_vm` is fail-closed (no Host fallback). |

| Key | Default | This box will… |
| --- | --- | --- |
| `run_host` | `true` | Accept host-process agent runtime |
| `run_container` | `false` | Accept container/VM agent runtime (docker or kube_pod) |
| `container_backend` | `k3d` | Hint: `docker` requires the docker CLI; `k3d` prefers kube |
| `docker_network` | `bridge` | Docker network for `docker run`. Empty omits `--network`. `none` blocks heartbeats. |

`advertised_container` is true when `run_container` is true **and** docker
or kube (in-cluster / `KUBECONFIG`) is available.

Host-mode children: cwd `{work_dir}/{session_id}`, env = job `env` +
`extra_env`. `extra_env` cannot override `FLEET_WORKER_TOKEN`.

Need `fleet-driver` on `PATH` for host mode (same install script; staged
next to `fleet-host`).

## Commands

```
fleet-host pair --url URL --token TOKEN
fleet-host run
fleet-host update
fleet-host status
fleet-host unpair
```

`auto_update` in `config.toml` defaults **true**. The plane
`host.auto_update` is authoritative; heartbeat copies it into
`config.toml` so the file stays a mirror. Auto apply runs after
heartbeat when that flag is on and `desired` bins differ; it never
sets CP `paused`/`drain`. `fleet-host update` is the same apply path,
forced.
