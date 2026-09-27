# Verify a self-hosted installation

An offline card on **Runners → Self-hosted** means that machine has not
sent a heartbeat for 90 seconds. The control plane never learns whether
the install was deleted, the service was disabled, or the process simply
stopped. Those facts are only on the computer.

Do not re-pair to find out. Re-pair rotates the machine token. The command
prints a verdict, pings the control plane, and can repair with the token
already on disk. Repair runs only if you answer y. It does not mint a token.

## On the card

Open the offline runner. **Verify installation** shows two commands. The
origin is `WORKER_PUBLIC_BASE` (the same origin the install command uses).
Paste the matching one in a terminal **on that computer**.

```sh
curl -fsSL "$ORIGIN/install-worker.sh" | FLEET_URL="$ORIGIN" sh -s -- verify
```

```powershell
$env:FLEET_URL="$ORIGIN"; $env:FLEET_ACTION="verify"; irm "$ORIGIN/install-worker.ps1" | iex
```

`repair` is the same script with `sh -s -- repair` (or `FLEET_ACTION=repair`). It keeps the machine token. `install` is the default and is the only action that uses a pair token.

The script does not pair and does not print `machine.token`. It pings
`/health` on the URL in `config.toml` and on `FLEET_URL` from the command.
When the program and the token are both present it asks whether to repair:
write that URL and restart the service, keeping the same token. The result
lines include:

```text
verdict:         service_disabled
next:            systemctl --user enable --now fleet-host
```

Run `next` yourself only when you want the service back.

## What the verdict means

| Verdict | What is on the machine | What to do |
| --- | --- | --- |
| `not_installed` | No binary, no config, no service file | **Add worker** and paste the install command. The card is a leftover row. |
| `bins_missing` | Config or a service file remains, `fleet-host` is gone | The install was removed. If the output says `token: present`, re-run the install command. A paired machine may reinstall without a new pair token. Do not re-pair. |
| `service_disabled` | Binary is there. systemd is `disabled` or `masked` (Windows: the logon startup entry is missing). Nothing is running | The install is intact. Run the printed `next` line. On a normal per-user Linux install that is `systemctl --user enable --now fleet-host`. |
| `service_stopped` | Installed, and the unit is enabled (or there is no unit). The process is down | Run the printed `next` line. Enabled but inactive is `systemctl --user start fleet-host`. |
| `service_running` | `fleet-host` is running on this computer | The offline card is not a missing install. The process is not reaching the control plane, or this terminal is a different machine than the card. Read `last_heartbeat` and the `url` in `~/.config/fleet-host/config.toml`. Do not re-pair. |

Exit status: `0` running, `2` disabled, `3` stopped, `4` not installed or bins missing.

## What this cannot answer

If you cannot open a terminal on that computer, the check cannot run.
The machine is off, or you are looking at the card from somewhere else.
That is a different fact from "the install was deleted."

A user systemd unit only answers inside a login session. If `systemctl`
does not answer, the script says so and does not invent `service_disabled`.

## Layout the script reads

Same paths the installer writes:

| OS | Binary | Service |
| --- | --- | --- |
| Linux, per user | `~/.local/bin/fleet-host` | `systemctl --user` unit `fleet-host` |
| Linux, root | `/usr/local/bin/fleet-host` | system unit `fleet-host` |
| macOS | `~/.local/bin/fleet-host` | `~/Library/LaunchAgents/ai.agentfleet.host.plist` |
| FreeBSD | `~/.local/bin/fleet-host` or `/usr/local/bin` | `/usr/local/etc/rc.d/fleet-host` |
| Windows | `%LOCALAPPDATA%\fleet-host\bin\fleet-host.exe` | Startup `fleet-host.cmd` |

Config and token, when present: `~/.config/fleet-host/config.toml` and
`machine.token`. A missing token with the binary still there is reported
as `token: missing` on the same verdict. It does not by itself mean the
install was deleted.
