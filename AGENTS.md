# Agent notes

## Parallelism

Use parallelism and subagents as much as possible while it is still
efficient and effective. Fan out independent reads, edits, and test
runs. Do not serialize work that does not depend on itself. Do not
spawn so many children that they collide on the same files or drown
the result in merge cost. Prefer a few sharp parallel tracks over one
long queue.

## Install

Install is one command, no arguments:

`crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`

Read that script for OS/RAM/disk/`PROFILE`/`ENABLE_*`. Persist those in the
repo-root `.env` (same file as DATABASE_URL / JWT / OAuth). Do not invent a
second install guide and do not pass flags on the command line.

## Automated tests

How they are organized and run: `docs/testing.md`.
Where to put new tests: `docs/testing-standardization.md`.

Before you commit: check whether the install script needs updating (new deps,
flags, OS/kind/lab behavior) and whether any tests must be added. No
regressions — run the relevant suite (`run-full-suite.sh` / cargo / lab
unittests) and do not leave install or tests worse than you found it.

## Fail-closed builds and identity

A missing tool, missing build, or foreign stack is a **fail**, not a WARN.

- Host `dx` is required. Kind UI is not a substitute. The install script
  must leave `dx` on PATH or exit 1 (Docker bookworm build if host OpenSSL
  cannot compile `openssl-sys`). Do not `cargo install` from bootstrap with
  a WARN-and-continue.
- Live identity is `scripts/tree-id.sh` (HEAD, plus a dirty suffix if the
  worktree has uncommitted changes). Gates require **both**
  `GET /health` `git_sha` **and** `GET /fleet-git-sha.txt` to equal that id.
- **After any code change, run the install script or rebuild**
  (`./scripts/run-every-test.sh --prepare-kind` and host `dx`) **before**
  treating live/e2e/human-test/every-test as valid. Verification without
  that step is not valid — images still labeled with the previous tree-id
  will fail identity (or worse, match HEAD while serving old bits).
- `/health.git_sha` is baked at **compile time** (`option_env!("FLEET_GIT_SHA")`).
  Never `kubectl set env FLEET_GIT_SHA` to satisfy identity. Never print
  EVERY-TEST OK with caveats.
- `:local` images skip rebuild only when label `fleet.git_sha` matches
  `scripts/tree-id.sh`.
- There is no green path via `FLEET_ALLOW_FOREIGN_STACK`.
- Contract: `tests/browser/test_fail_closed_contract.py`.

This GitLab has no runner. Do not add `.gitlab-ci.yml`, GitLab runners, or
any pipeline that GitLab would create on push or MR. Pending jobs block
merge ("pipelines must succeed" + nothing ever starts). Run tests locally.

## Human testing

Protocol (checklist, when to film, which scene maps to which crate):
`docs/human-testing.md`.

- **Humans** run `./scripts/human-test.sh`. It starts Postgres + API (:8080)
  + UI (:3030) if needed and walks every current surface. Use it when you
  do not know the product yet, and again whenever you add a surface.
- Port-up is not this tree. Live gates require `GET /health` `git_sha` **and**
  UI `/fleet-git-sha.txt` == `scripts/tree-id.sh` (Kind extraPortMappings
  often steal :8080/:3030). Rebuild after edits or the result is not valid.
- When you add a user-visible or operator-visible path, add a step to
  `scripts/human-test.sh` and a scene to `tests/browser/record_demo.py` in
  the **same** change.

## Merge requests and review

Ship work through a **GitLab merge request** and **ask a human peer to
review**. That is the default.

Skip the MR/review loop **only** for small tweaks: typos, comment nits,
import order, one-line docs wording. If it changes behavior, data, UI,
install, tests, sandbox/lab, or operator scripts, it is not a small tweak
— open an MR and name a reviewer.

Do not merge unless a human asks you to merge.

## Demo video (agent records it)

The **coding agent** records the demo, not the human.

1. Launch if needed: `./scripts/human-test.sh launch`
2. Film the scenes your diff can break (see the crate → scene table in
   `docs/human-testing.md`):
   `./scripts/human-test.sh record`  
   or `./scripts/human-test.sh record fleet chat`
3. Attach the `.webm` under `.tmp/demos/` to the GitLab MR. One sentence
   in the description: what the clip shows.
4. **Never** `git add` videos, tokens, or personal emails. Use
   `*@example.test` in recordings.

Every MR that is not a small tweak needs this clip. If Playwright cannot
launch, that is a fail — fix OS deps / stack; do not skip the video.
