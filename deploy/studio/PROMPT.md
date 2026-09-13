# Timer turn — agent-fleet VPS deploy

You are the scheduled Grok session that deploys Agent Fleet to
**us-vmmedium**. Finish the work below, then stop. Do not wait for a human.

Cwd is this directory (`deploy/studio`). Product tree is the repo root
(two levels up). Read `AGENTS.md` here and `AGENTS.md` at the repo root
(external connections / identity) before you ship anything.

The runtime header above this file (written by `spawn-deploy-session.sh`)
has the SHA, last deployed SHA, previous grok status, and state paths.

## Goal

`origin/main` of this repo must be live at
https://agentfleet.studio. Both of these must equal that commit:

- `GET https://agentfleet.studio/health` → `status=ok`, `git_sha=<sha>`
- `GET https://agentfleet.studio/fleet-git-sha.txt` → `<sha>`

## Steps

1. Read `~/.local/state/vps-fleet/grok-report` if it exists (prior turn).
2. Confirm `git fetch origin main` and `git rev-parse origin/main` in
   the product repo (header `product:`) match the header SHA.
   Do **not** checkout that SHA onto the operator worktree.
3. If live already matches that SHA and `/health` is ok: write
   `grok-status=deployed`, a one-line `grok-report`, and exit 0.
4. Ship with the existing script (this is the only ship path):

   ```
   ./scripts/deploy.sh
   ```

   It archives `origin/main`, builds `agent-fleet-api:<sha>` +
   `agent-fleet-ui:<sha>`, zstd-pipes them to the VPS, compose-ups, waits
   for health, rolls back on a bad SHA. First compile of a new SHA can
   take tens of minutes. Stay with it.
5. On success: write `grok-status=deployed` and a short `grok-report`
   (SHA, public health). Stop.
6. On **docker build / compile** failure:
   - Diagnose from the journal / docker output. Do not retry the same
     broken archive in a loop.
   - Fix in a **new worktree** of the product repo based on `origin/main`.
     Never edit the operator checkout in place.
   - Push a branch and open a GitLab MR (`glab` if present). Do not
     merge it yourself. Do not force-push `main`.
   - Leave production on the last good SHA (`deploy.sh` already did not
     ship a failed build).
   - Write `grok-status=blocked` and `grok-report` with the compile
     errors, the MR URL, and what a human should do. Stop.
7. On **health / identity** failure after a ship: `deploy.sh` should have
   rolled back. Confirm live is the previous SHA and healthy. If rollback
   also failed, say so in `grok-report`, write `grok-status=blocked`, stop.
   Do not rotate tokens or re-pair runners to "fix" it.
8. If you cannot make progress without a human (auth, disk, GitLab down,
   missing context): `grok-status=blocked` + report. Stop.

## State files (write these)

Directory: `~/.local/state/vps-fleet/`

| File | Value |
|---|---|
| `grok-status` | one line: `deployed` / `blocked` / `failed` |
| `grok-report` | short human summary, no secrets |
| `grok-session` | leave as the wrapper wrote it |

`deployed-sha` is owned by `deploy.sh` after a successful ship. Do not
invent a SHA there.

## Do not

- Mutate the operator worktree HEAD, index, or dirty files
- `DEPLOY_SOURCE=worktree` / `FORCE=1` unless live is down **and** the
  SHA you would ship is already on GitLab `main`
- Ship a tree-id with a dirty suffix (`<sha>-<hash>`)
- Rotate pair tokens, wipe `machine.id`, force a re-pair
- Restage worker dist without confirming the new `fleet-host` starts
- Change `API_PUBLIC_BASE` / `WORKER_PUBLIC_BASE` / OAuth as a side effect
- Wait around after you have deployed, opened an MR, or blocked
