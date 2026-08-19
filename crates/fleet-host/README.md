# fleet-host

Paired machine agent. Outbound HTTPS only. Pairs with a control plane, then
heartbeats and leases `spawn_session` jobs.

Config lives on the box and is **never** accepted from the control plane:

```
~/.config/fleet-host/config.toml     # 0700 directory
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

```bash
fleet-host status    # host_id, url, run_host, run_container, token present
fleet-host run       # heartbeat + lease loop (systemd / tmux / foreground)
fleet-host unpair    # delete machine.token only; config stays
```

## Host vs container

v1 is **host mode only**. `run_container` / k3d is not supported; container
execution is not validated. Plane `allow_container` may stay true as a
restriction only.

Effective run mode is the intersection of plane flags and **this file**.
The plane can only restrict further.

| Key | Default | This box will… |
| --- | --- | --- |
| `run_host` | `true` | Exec `fleet-worker` (or `fleet-driver` when the job has `FLEET_HARNESS`) on the host |
| `run_container` | `false` | **Not supported in v1.** Would spawn a one-shot container / k3d pod |
| `container_backend` | `k3d` | **Not supported in v1.** `k3d` or `docker` |
| `docker_network` | `bridge` | Docker network for `docker run`. Empty omits `--network`. `none` isolates the container. |

`advertised_container` is true only when `run_container` is true **and** the
backend binary (`k3d` or `docker`) is on `PATH`. That advertisement is not
a working executor in v1.

Host-mode children: cwd `{work_dir}/{session_id}`, env = job `env` +
`extra_env`. `extra_env` cannot override `FLEET_WORKER_TOKEN`.

Need `fleet-worker` / `fleet-driver` on `PATH` for host mode (same install
script). Do not enable `run_container` expecting k3d or Docker jobs to work.

## Commands

```
fleet-host pair --url URL --token TOKEN
fleet-host run
fleet-host status
fleet-host unpair
```
