# deploy/studio

VPS ship for **us-vmmedium**. Lives in this product repo. Every 5 minutes a systemd
`OnCalendar` timer fetches GitLab `main` and, if that SHA is not already live, **spawns
a Grok session** to build images and ship them over SSH. (`Persistent=` only
works with `OnCalendar`; do not switch back to `OnUnitActiveSec` or a reboot
leaves the timer elapsed with no next fire.) The VPS never
gets a GitLab key. `scripts/deploy.sh` is still the ship tool — Grok is
what decides to run it, diagnose a red build, and open a fix MR.

| | |
|---|---|
| App | this repo `origin/main` |
| VPS | `agent-fleet@82.39.213.33` — see [ssh.md](ssh.md) |
| UI | https://agentfleet.studio |
| Stack dir | `/home/agent-fleet/stack` |
| Secrets | `/home/agent-fleet/stack/.env` (created once, never overwritten) |

## First time

```bash
./scripts/bootstrap-vps.sh          # docker group, stack dir, generate .env on the VPS
./scripts/spawn-deploy-session.sh   # fetch main; spawn Grok if SHA is not live
./scripts/install-timer.sh          # every 5 minutes after that
```

The first ship compiles the Rust API + UI images. Expect tens of minutes.
Later ticks no-op unless `origin/main` moved (they do **not** start Grok).
A desktop notification on this PC fires on a successful ship and on failure
(build, health, rollback).

Worker packages (`GET /install/worker/bin/{platform}/{bin}`) are staged in
the same `deploy.sh` pass: **linux-x86_64** (bookworm), **linux-aarch64**,
**windows-x86_64**. A platform is rebuilt only when `fleet-host` /
`fleet-driver` sources changed since the staged manifest. Unchanged
sources keep the bins already staged (compiles are not bit-identical).
musl, armv7, and windows-arm64 are not built. **darwin-*** is not built
here — this box cannot cross-compile macOS.

The author bumps `[versions]`, `[verify]`, and the per-OS rev. This
script writes the sha256 from the bins it built (`register
--keep-author-revs`) and stores
`~/.local/state/vps-fleet/runner-packages.followup.toml`. Open a
follow-up MR with that file. Do not merge it from the deploy session.

An MR is mergeable only after a ship proof for that commit:

```bash
PROVE_ONLY=1 DEPLOY_SHA=<commit> ./scripts/deploy.sh
```

That builds both images and the three platforms and writes
`~/.local/state/vps-fleet/ship-proof/<commit>`. It does not contact the VPS.

## Day to day

```bash
./scripts/spawn-deploy-session.sh        # spawn Grok if main is not live
DRY_RUN=1 ./scripts/spawn-deploy-session.sh
FORCE=1 ./scripts/spawn-deploy-session.sh   # spawn even if already live
./scripts/deploy.sh                      # ship without Grok (manual)
FORCE=1 ./scripts/deploy.sh              # rebuild this SHA anyway
systemctl --user start agent-fleet-deploy.service
journalctl --user -u agent-fleet-deploy.service -f
```

Session on disk: `~/.local/state/vps-fleet/grok-session`. Resume in the TUI:

```bash
cd deploy/studio
grok --resume "$(cat ~/.local/state/vps-fleet/grok-session)"
```

A tick skips Grok when live already matches `origin/main`, or when a prior
session wrote `grok-status=blocked` for that SHA (compile error waiting on
an MR). `FORCE=1` retries a blocked SHA.

Rollback: if the new SHA fails `/health` + `/fleet-git-sha.txt`, `deploy.sh`
brings back the previous image tags automatically.

## What the VPS can see

- Prebuilt `agent-fleet-api:<sha>` and `agent-fleet-ui:<sha>`
- `compose.yml` (image names only, no `build:`)
- Its own `.env`

It never clones the repo and never talks to GitLab.

## Domain (Cloudflare)

Public name is **https://agentfleet.studio**. Caddy on the VPS listens on :80 and :443.
The domain currently resolves to Cloudflare, not the VPS — point it here:

1. Cloudflare → DNS → A `agentfleet.studio` → `82.39.213.33` (proxied)
2. A `www` → `82.39.213.33` (proxied)
3. SSL/TLS → **Full** (not Flexible, not Full strict)

Leave the records orange-clouded. Visitors use Cloudflare’s cert; origin uses a self-signed cert on :443. HTTP :80 redirects to HTTPS (requires SSL mode **Full**).

## Hardening

```bash
./scripts/harden-vps.sh          # key-only SSH, UFW, fail2ban; keeps admin sudo
./scripts/install-deploy-key.sh  # restricted deploy user (no docker group, no shell sudo)
```

Published 80/443 stay Cloudflare-only via `fleet-docker-fw.service`
(`/usr/local/sbin/fleet-docker-fw`, source [`scripts/fleet-docker-fw.sh`](scripts/fleet-docker-fw.sh)).
That chain must **RETURN** RFC1918 sources before the dport 80/443 DROP, or
the API cannot reach Slack/GitHub/Grok (`oauth.v2.access` times out).

Deploys use `~/.ssh/agent-fleet-deploy`. Admin/break-glass stays `~/.ssh/agent-fleet`.

Signup requires `SIGNUP_TOKEN` from `/home/agent-fleet/stack/.env` (field on
`/signup`). Login/signup are rate-limited per IP.

## Email

Inbound: Cloudflare Email Routing (`hello@agentfleet.studio` → your inbox).
Outbound: Resend from `noreply@mail.agentfleet.studio`. Setup: [mail.md](mail.md).
