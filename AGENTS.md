# Agent notes

## External connections (do not lock operators out)

Paired runners and live integrations are **external connections**. Treat them
like a production machine you cannot walk over to.

Anything that can drop a heartbeat, rotate a pair token, change
`WORKER_PUBLIC_BASE` / `control_plane_url`, serve a new `fleet-host` package,
or flip OAuth / API public bases **must be done so the remote box can still
phone home with the token it already has**.

- Do **not** force a re-pair as the fix. Re-pair is a last resort the human
  runs on that machine. The durable `machine.id` is meant to reclaim the same
  row; wiping tokens or serving a host binary that never comes back is a
  lockout.
- Auto-update of a paired runner is a live code push to someone else's box.
  If you restage `FLEET_WORKER_DIST_DIR` and `auto_update` is on, that host
  will download and replace itself. Confirm the package starts **before**
  another restage. If a runner goes silent mid-update, turn **off** CP
  `auto_update` for that row so a restart of the last working binary is not
  immediately overwritten. Do not rotate its machine token.
- Debug Windows/Linux worker packages are huge and easy to brick a laptop
  with. Prefer a known-good staged dist over “ship whatever just compiled.”
- Integration OAuth (`API_PUBLIC_BASE`) and worker pair URL
  (`WORKER_PUBLIC_BASE`) are different. Changing the wrong one breaks Connect
  or every runner, not both in a useful way.

If you would not do it to a customer’s only jump host, do not do it to X1.

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

A recurring test-writer is allowed. Treat each test as **independent**.
Do **not** re-run the entire suite on every iteration — only the tests
that fire added, changed, or that failed last time. After a **new code
version** (`scripts/tree-id.sh` changed), those tests should be run
again; a dirty `tree-id` from editing a file is not a reason to re-run
everything. Harvest from `just test`, `just test-integration`,
`scripts/harvest-runner-catalog.sh`, Playwright. An explicit skip is
`LIVE_SKIPPED`.

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

This GitLab has no runner. Keep `.gitlab-ci.yml` as `workflow: rules:
- when: never` only. Do not add jobs, runners, or Auto DevOps. A missing
file or a real job creates a pending pipeline that blocks merge. Run
tests locally.

## Human testing

Protocol (checklist, when to film, which scene maps to which crate):
`docs/human-testing.md`.

- **Humans** run `./scripts/human-test.sh`. It starts Postgres + API (:8080)
  + UI (:3030) if needed and walks **HUMAN_TEST_SCOPE.md** (not the full
  catalog). `guide-all` is a first product tour only. After any code
  change, the developer AI runs
  `python3 tests/browser/surface_scope.py --write` so the scope file
  lists every surface the diff can break (direct + indirect path hits).
  The MR must include that file; `--check` must pass.
- Port-up is not this tree. Live gates require `GET /health` `git_sha` **and**
  UI `/fleet-git-sha.txt` == `scripts/tree-id.sh` (Kind extraPortMappings
  often steal :8080/:3030). Host `fleet-dev` then uses :18080/:13030.
  Rebuild the sha-consuming bits after edits or the result is not valid.
- When you add a user-visible or operator-visible path, add a step to
  `scripts/human-test.sh` and a scene to `tests/browser/record_demo.py` in
  the **same** change.

## Merge requests and review

Ship work through a **GitLab merge request** and **ask for a peer review on
Slack**. That is the default.

Skip the MR/review loop **only** for small tweaks: typos, comment nits,
import order, one-line docs wording. If it changes behavior, data, UI,
install, tests, sandbox/lab, or operator scripts, it is not a small tweak
— open an MR and ask on Slack.

Do not merge unless a human asks you to merge.

All tests must pass before a merge to main. `just test` and
`just test-integration` must be green on the merge tree. Do not merge
with a red test, an ignored failure, `|| true` around `cargo test`, or
a skip that hides a broken path. This GitLab has no runner — the lock
is local (and the source-scan in `fleet-server` `merge_gate`).

The last compile and test pass before you open the MR must run **in the
ship image**, not only on the host. VPS and Kind build
`Dockerfile` / `Dockerfile.ui` (`rust:1.88-bookworm`). Host `cargo` /
`just test` can be green while that image does not compile (OpenSSL,
glibc, sqlx migration versions, locked bins). That is how a tree that
cannot ship reached `main`.

```bash
docker build --build-arg FLEET_GIT_SHA="$(scripts/tree-id.sh)" -f Dockerfile .
docker build --build-arg FLEET_GIT_SHA="$(scripts/tree-id.sh)" -f Dockerfile.ui .
```

`./scripts/run-every-test.sh --prepare-kind` already does those builds.
A host-only green is not enough to open the MR.

## Demo video (agent records it)

The **coding agent** records the demo, not the human.

Film the **final tree that will be merged**: all edits done, identity
(`GET /health` `git_sha` **and** UI `/fleet-git-sha.txt`) ==
`scripts/tree-id.sh` for that tree. A clip shot before later edits, or
against an older bake, is **not** MR proof — discard it and re-film.
Do not film, then change code, then attach the old `.webm`.

The clip must show **every surface in HUMAN_TEST_SCOPE.md** (not a
random walkthrough, not a full-app tour). After the recording, the
agent **must review the film**: open each `.tmp/demos/<stamp>-frames/*.png`
and the `.webm`, look for bugs (wrong heading, error toast, missing
control, broken layout, failed action, leaked secret), and fill the
**Film review** section in `HUMAN_TEST_SCOPE.md` before attaching.

1. Finish the change. Rebuild if you edited after the last bake.
2. `python3 tests/browser/surface_scope.py --write` (and `--check`).
3. Launch if needed: `./scripts/human-test.sh launch`
4. Film the scope: `./scripts/human-test.sh record`
5. Review frames + `.webm` for bugs; fill HUMAN_TEST_SCOPE.md Film review.
6. Attach the `.webm` to the GitLab MR. One sentence: which scenes + review.
7. **Never** `git add` videos, tokens, or personal emails. Use
   `*@example.test` in recordings.

Every MR that is not a small tweak needs this clip. If Playwright cannot
launch, that is a fail — fix OS deps / stack; do not skip the video.
