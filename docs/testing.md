# Testing

How this repo is tested. Install is still one script
(`crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`)
and repo-root `.env`. This page only says **how to run tests**.
Where a new test belongs: [`testing-standardization.md`](./testing-standardization.md).

## No GitLab CI

This GitLab has no runner. `.gitlab-ci.yml` is only `workflow: rules:
- when: never` so GitLab does not create a pending pipeline. Do not add
jobs or a runner. Tests run on a developer or agent host:

`just test`, `just test-integration`, `run-full-suite.sh`.

## Layers

| Layer | Where | Command | Skip? |
| --- | --- | --- | --- |
| Unit | `#[cfg(test)]` next to Rust code (includes `fleet-chat-md`) | `just test` → `cargo test --workspace --lib --exclude fleet-ui --locked` | Never |
| Process worker | `crates/fleet-driver/tests/` (`acp_mock`, `worker_protocol`) | same `just test` → `cargo test -p fleet-driver --locked` | Never (`acp_grok` is `#[ignore]`) |
| Rust integration | `fleet-orchestrator` / `fleet-auth` / `fleet-server` / `fleet-db` `tests/` | `just test-integration` (needs Postgres via `just db-up`) | **Fail** if `DATABASE_URL` is unset |
| Lab | `sandbox-lab/**/test_*.py` | `run-full-suite.sh` | Skip only when the matching `ENABLE_*` is off; **fail** if the flag is on and the service is down |
| Browser | `tests/browser/test_app_e2e.py` | suite stage when `ENABLE_BROWSER_E2E=1` (needs `:3030` + `:8080`; after kind when `ENABLE_KIND=1`) | Off unless enabled; then **fail** if the stack is down |
| Runner guests | `sandbox-lab/tests/test_runner_guest_e2e.py` | `ENABLE_RUNNER_GUEST_E2E=1` + this tree + a guest per OS | Default **on** for every `PROFILE`. Missing guest/stack is a **fail**. Explicit `0` still wins. Live **skipped for now:** linux ARM, `freebsd-amd64`, Darwin, `windows-arm64` (`LIVE_SKIPPED`). Procedure: [`components/runners/testing-procedures.md`](./components/runners/testing-procedures.md) |
| Human walkthrough | `scripts/human-test.sh` | interactive; starts the stack | Humans, not CI. For an MR, only the steps that merge can break |
| MR demo video | `scripts/human-test.sh record <scenes>` (agent, Playwright `.webm`) | attach to the GitLab MR | Final merge tree; affected scenes only; never commit the file |
| Live LLM / Grok ACP | `#[ignore]` | `cargo test -p fleet-driver -- --ignored` | Ignored by default |

`fleet-ui` is a Dioxus/WASM app; it is excluded from `cargo test --workspace --lib`. Cover chrome with Playwright. Chat markdown units live in `fleet-chat-md` and runner card helpers live in `fleet-runners-view`; both run on `just test`.

`ENABLE_BROWSER_E2E` defaults **on** only for `PROFILE=full` (kind publishes UI/API). `lab` and `minimal` default it off.

Live `:3030`/`:8080` is **this worktree** only when **both** `/health.git_sha`
and UI `/fleet-git-sha.txt` equal `scripts/tree-id.sh` (HEAD, or
`HEAD-<dirty>` if you have uncommitted changes). The API sha is baked at
compile time — `kubectl set env` cannot fake it. After any edit, rebuild
the **sha-consuming** bits (host `dx` + `cargo build -p fleet-server`, or
`--prepare-kind`). Docker dep compile is cached: tree-id is not the
crates.io cache key. `:local` images skip rebuild only when label
`fleet.git_sha` matches `scripts/tree-id.sh`. Kind extraPortMappings often
own `:8080`/`:3030` with **old** images — port-up is not enough; host
`fleet-dev` then binds `18080`/`13030` (`FLEET_API_URL` / `FLEET_UI_URL`).
`tests/browser/stack_identity.py` fails closed. There is no green path via
`FLEET_ALLOW_FOREIGN_STACK`. Missing host `dx` is a fail (Kind UI is not a
substitute). Kind videos filmed without matching API+UI ids are not proof.
Contract: `tests/browser/test_fail_closed_contract.py`.

## Harvest, not a writer loop

Catalog status comes from running the commands above (or
`scripts/harvest-runner-catalog.sh`). Do not schedule a recurring agent
that implements missing rows or relaunches children. Do not spawn one
agent per catalog row on a timer. A missing child id is not a relaunch.
One stack rebuild, then one e2e pass. `LIVE_SKIPPED` guests stay skipped
until there is a bootable guest.

## Rules

- Before you open an MR, the last compile and test pass must run **in the
  ship image** (`Dockerfile` / `Dockerfile.ui`, `rust:1.88-bookworm`), not
  only host `cargo` / `just test`. Host-only green is how a tree that does
  not build in VPS/Kind reaches `main`.
  `docker build --build-arg FLEET_GIT_SHA="$(scripts/tree-id.sh)" -f Dockerfile .`
  (and `-f Dockerfile.ui`); `./scripts/run-every-test.sh --prepare-kind`
  already does both.
- Rust behavior is tested in Rust. Python tests the Python workspace agent and compose/kind YAML.
- No silent `eprintln!("skip"); return;` in `crates/**/tests/*.rs`. Missing `DATABASE_URL` panics with a one-line how-to.
- Do not use `|| true` around `cargo test`.
- `SKIP_*` on `run-full-suite.sh` is for operators who cannot run compose/kind/LLM, not a way to hide failures.
- Do not add GitLab jobs or runners. Keep `.gitlab-ci.yml` as `when: never`.
- Human testing and agent-recorded MR demos: [`human-testing.md`](./human-testing.md).
- Behavior catalog: [`behavior-interactions.md`](./behavior-interactions.md). HTTP/auth/fleet coverage lives in crate integration tests, not a second Python framework.
- Runner behaviors require unit + browser e2e + real-life VM/OS e2e: [`components/runners/testing-requirements.md`](./components/runners/testing-requirements.md). That file is the requirement, not the tests.

## Commands

```bash
just test                          # no Postgres
just test-integration              # just db-up + DATABASE_URL from .env.local
crates/fleet-sandbox-driver/sandbox-lab/scripts/run-full-suite.sh
./scripts/run-every-test.sh         # every bucket; no SKIP_* (see bootstrap-every-test.sh)
```

Live Grok ACP (optional):

```bash
cargo test -p fleet-driver --test acp_grok -- --ignored --nocapture
```
