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

## Isolation vs placement

Isolation is `process` < `container` < `vm`. k8s is **placement**, not
isolation. A pod is still a container (`KubePod`). Firecracker / QEMU /
KubeVirt are `vm`.

| Isolation | Executor | This box will… |
| --- | --- | --- |
| `process` (`runtime=host`) | exec | `fleet-driver` on the host (`FLEET_HARNESS=mock\|grok`) |
| `container` | docker | `docker run` (`--network` default `none`). Same `FLEET_*` as host. Missing docker is an error — never Host. |
| `container` | kube_pod | In-cluster (or `KUBECONFIG`) creates a `FleetSandbox` CR. Guest is `fleet-driver` (ACP). |
| `vm` | firecracker / qemu | Allowed only when a hypervisor is on `PATH`. `run_vm` is fail-closed (no Host fallback). |

| Key | Default | This box will… |
| --- | --- | --- |
| `run_host` | `true` | Accept process isolation |
| `run_container` | `false` | Accept container isolation (docker or kube_pod) |
| `container_backend` | `k3d` | Hint: `docker` requires the docker CLI; `k3d` prefers kube |
| `docker_network` | `bridge` | Docker network for `docker run`. Empty omits `--network`. `none` blocks heartbeats. |

`advertised_container` is true when `run_container` is true **and** docker
or kube (in-cluster / `KUBECONFIG`) is available.

Host-mode children: cwd `{work_dir}/{session_id}`, env = job `env` +
`extra_env`. `extra_env` cannot override `FLEET_WORKER_TOKEN`.

Need `fleet-worker` / `fleet-driver` on `PATH` for host mode (same install
script).

## Commands

```
fleet-host pair --url URL --token TOKEN
fleet-host run
fleet-host update
fleet-host status
fleet-host unpair
```

`auto_update` in `config.toml` defaults **true**. Effective auto is that
flag **and** the plane `host.auto_update` (also default true). Auto apply
runs after heartbeat when `desired` bins differ; it never sets CP
`paused`/`drain`. `fleet-host update` is the same apply path, forced.
