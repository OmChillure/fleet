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
- Ship proof, before merge: `PROVE_ONLY=1 DEPLOY_SHA=<commit> ./scripts/deploy.sh` (no VPS contact)
- Runner revs: the author bumps them before merge. If a ship platform's bytes changed and the rev was not bumped, this script bumps it, commits that onto `main`, and deploys that commit. It does not force-push.
- Live: https://agentfleet.studio
- Identity: `GET /health` `git_sha` **and** `GET /fleet-git-sha.txt` must equal the shipped commit
- State: `~/.local/state/vps-fleet/` (`deployed-sha`, `grok-status`, `grok-session`, `grok-report`)

## Hard rules

- This checkout is the only product folder. Do **not** create another
  worktree or another directory under `/code`. `deploy.sh` reuses
  `~/.local/state/vps-fleet/build` and keeps its `target/`.
- Do **not** `git checkout`, `reset`, or `clean` this checkout. Fetch is fine.
- Do **not** deploy a dirty / local-only tree. Production SHA must be a
  GitLab commit. `DEPLOY_SOURCE=worktree` is not for this timer.
- Do **not** force-push `main`. Fixes go on a branch + GitLab MR.
- Do **not** rotate pair tokens, wipe `machine.id`, or force a re-pair.
- Do **not** restage `FLEET_WORKER_DIST_DIR` unless the new `fleet-host`
  package is known to start. Auto-update on a paired runner is a live push
  to someone else's box.
- If you cannot land this SHA and cannot open a fix MR, write
  `grok-status=blocked` and stop. Do not loop.
