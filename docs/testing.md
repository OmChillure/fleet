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
| Human walkthrough | `scripts/human-test.sh` | interactive; starts the stack | Humans, not CI |
| MR demo video | `scripts/human-test.sh record` (agent, Playwright `.webm`) | attach to the GitLab MR | Agent records; never commit the file |
| Live LLM / Grok ACP | `#[ignore]` | `cargo test -p fleet-driver -- --ignored` | Ignored by default |

`fleet-ui` is a Dioxus/WASM app; it is excluded from `cargo test --workspace --lib`. Cover chrome with Playwright. Chat markdown units live in `fleet-chat-md` and run on `just test`.

`ENABLE_BROWSER_E2E` defaults **on** only for `PROFILE=full` (kind publishes UI/API). `lab` and `minimal` default it off.

Live `:3030`/`:8080` is **this worktree** only when **both** `/health.git_sha`
and UI `/fleet-git-sha.txt` equal `scripts/tree-id.sh` (HEAD, or
`HEAD-<dirty>` if you have uncommitted changes). The API sha is baked at
compile time — `kubectl set env` cannot fake it. After any edit, run the
install script or `./scripts/run-every-test.sh --prepare-kind` (and host
`dx`) **before** verification; a run without that rebuild is not valid.
Kind extraPortMappings often own those ports with **old** images — port-up
is not enough. `tests/browser/stack_identity.py` fails closed. There is no
green path via `FLEET_ALLOW_FOREIGN_STACK`. Missing host `dx` is a fail
(Kind UI is not a substitute). Kind videos filmed without matching API+UI
ids are not proof. Contract: `tests/browser/test_fail_closed_contract.py`.

## Rules

- Rust behavior is tested in Rust. Python tests the Python workspace agent and compose/kind YAML.
- No silent `eprintln!("skip"); return;` in `crates/**/tests/*.rs`. Missing `DATABASE_URL` panics with a one-line how-to.
- Do not use `|| true` around `cargo test`.
- `SKIP_*` on `run-full-suite.sh` is for operators who cannot run compose/kind/LLM, not a way to hide failures.
- Do not add GitLab jobs or runners. Keep `.gitlab-ci.yml` as `when: never`.
- Human testing and agent-recorded MR demos: [`human-testing.md`](./human-testing.md).
- Behavior catalog: [`behavior-interactions.md`](./behavior-interactions.md). HTTP/auth/fleet coverage lives in crate integration tests, not a second Python framework.

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
