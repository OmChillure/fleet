# us-vmmedium

| | |
|---|---|
| Host | `82.39.213.33` |
| Hostname | `us-vmmedium` |
| User | `agent-fleet` |
| OS | Ubuntu 26.04.1 LTS |
| Admin key | `~/.ssh/agent-fleet` → user `agent-fleet` — passwordless sudo |
| Deploy key | `~/.ssh/agent-fleet-deploy` → user `agent-fleet-deploy` — only `sudo fleet-stack` |
| Guest admin | user `ochillure` — passwordless sudo, key held by the holder (not on this box) |

```bash
ssh -i ~/.ssh/agent-fleet agent-fleet@82.39.213.33
ssh -i ~/.ssh/agent-fleet-deploy agent-fleet-deploy@82.39.213.33
```

```
Host us-vmmedium
  HostName 82.39.213.33
  User agent-fleet
  IdentityFile ~/.ssh/agent-fleet
  IdentitiesOnly yes

Host us-vmmedium-deploy
  HostName 82.39.213.33
  User agent-fleet-deploy
  IdentityFile ~/.ssh/agent-fleet-deploy
  IdentitiesOnly yes
```

Admin public key:

```
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMnI9ENz2wMdjUCKYd2Y9s0lNz8Eo+v0WBZjJQnwMOxP agent-fleet
```

Fingerprint: `SHA256:FZe5AIi/rxTof++NK9Ehvc5h1OZMM9SBbc4gkY4kDsw`

Deploy public key:

```
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILx9Oq1ig7oDilsInYA78M91s7YJzgxvEqulSA39qmwK agent-fleet-deploy
```

## Guest admin: `ochillure`

Added 2026-09-03. Own account, `sudo` group + `/etc/sudoers.d/ochillure`
(`NOPASSWD:ALL`). Not in `docker`. No expiry set. Private key is held by the
holder — it was never on this box.

```
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIK0KfD7W/QGdXLn476j/9jATzLFwxnlWB5V2i4lUDqRT ochillure@ext.teachx.ai
```

Fingerprint: `SHA256:f8bNrsNdoxYvKyPZOHSXBSuUfiY3yvsIH/23L1TPZ30`

Revoke:

```bash
ssh -i ~/.ssh/agent-fleet agent-fleet@82.39.213.33 \
  'sudo rm -f /etc/sudoers.d/ochillure && sudo deluser --remove-home ochillure'
```

Stack: `/home/agent-fleet/stack` (see [README.md](README.md)). UI: https://agentfleet.studio
