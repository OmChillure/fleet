# deploy/studio

This directory is the CI/CD for **us-vmmedium**. A systemd user timer
spawns a Grok session (`scripts/spawn-deploy-session.sh`) when GitLab
`origin/main` is not what https://agentfleet.studio is serving.

It lives in the product repo (`deploy/studio/`). Runtime state stays
under `~/.local/state/vps-fleet/` (not in git).

## This session's job

Deploy **agent-fleet** `origin/main` to the VPS. Then stop.

- Product checkout: repo root (parent of `deploy/studio`)
- Ship tool: `deploy/studio/scripts/deploy.sh` (only way images reach the VPS)
- Live: https://agentfleet.studio
- Identity: `GET /health` `git_sha` **and** `GET /fleet-git-sha.txt` must equal the shipped commit
- State: `~/.local/state/vps-fleet/` (`deployed-sha`, `grok-status`, `grok-session`, `grok-report`)

## Hard rules

- Do **not** `git checkout`, `reset`, `clean`, or otherwise mutate the operator
  worktree. Fetch is fine. Edits go in a **new git worktree**.
- Do **not** deploy a dirty / local-only tree. Production SHA must be a
  GitLab commit. `DEPLOY_SOURCE=worktree` is not for this timer.
- Do **not** force-push `main`. Fixes go on a branch + GitLab MR.
- Do **not** rotate pair tokens, wipe `machine.id`, or force a re-pair.
- Do **not** restage `FLEET_WORKER_DIST_DIR` unless the new `fleet-host`
  package is known to start. Auto-update on a paired runner is a live push
  to someone else's box.
- If you cannot land this SHA and cannot open a fix MR, write
  `grok-status=blocked` and stop. Do not loop.
