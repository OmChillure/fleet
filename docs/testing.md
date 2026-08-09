# Testing

How this repo is tested. Install is still one script
(`crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`)
and repo-root `.env`. This page only says **how to run tests**.

## No GitLab CI

This GitLab has no runner. Do **not** add `.gitlab-ci.yml`, GitLab runners,
or any pipeline config. A pending pipeline blocks MR merge ("pipelines must
succeed" + the job never starts). Tests run on a developer or agent host:

`just test`, `just test-integration`, `run-full-suite.sh`.

## Layers

| Layer | Where | Command | Skip? |
| --- | --- | --- | --- |
| Unit | `#[cfg(test)]` next to Rust code | `just test` → `cargo test --workspace --lib --exclude fleet-ui --locked` | Never |
| Process worker | `crates/fleet-driver/tests/` (`acp_mock`, `worker_protocol`) | same `just test` → `cargo test -p fleet-driver --locked` | Never (`acp_grok` is `#[ignore]`) |
| Rust integration | `fleet-orchestrator` / `fleet-auth` / `fleet-server` `tests/` | `just test-integration` (needs Postgres via `just db-up`) | **Fail** if `DATABASE_URL` is unset |
| Lab | `sandbox-lab/**/test_*.py` | `run-full-suite.sh` | `skipUnless` only for optional kind / git-ssh |
| Browser | `tests/browser/test_app_e2e.py` | suite stage when `ENABLE_BROWSER_E2E=1` (needs `:3030` + `:8080`; after kind when `ENABLE_KIND=1`) | Off unless enabled; then **fail** if the stack is down |
| Live LLM / Grok ACP | `#[ignore]` | `cargo test -p fleet-driver -- --ignored` | Ignored by default |

`fleet-ui` is a Dioxus/WASM app; it is excluded from `cargo test --workspace --lib`. Cover it with Playwright.

`ENABLE_BROWSER_E2E` defaults **on** only for `PROFILE=full` (kind publishes UI/API). `lab` and `minimal` default it off.

## Rules

- Rust behavior is tested in Rust. Python tests the Python workspace agent and compose/kind YAML.
- No silent `eprintln!("skip"); return;` in `crates/**/tests/*.rs`. Missing `DATABASE_URL` panics with a one-line how-to.
- Do not use `|| true` around `cargo test`.
- `SKIP_*` on `run-full-suite.sh` is for operators who cannot run compose/kind/LLM, not a way to hide failures.
- Do not add GitLab CI or runners back.
- Behavior catalog: [`behavior-interactions.md`](./behavior-interactions.md). HTTP/auth/fleet coverage lives in crate integration tests, not a second Python framework.

## Commands

```bash
just test                          # no Postgres
just test-integration              # just db-up + DATABASE_URL from .env.local
crates/fleet-sandbox-driver/sandbox-lab/scripts/run-full-suite.sh
```

Live Grok ACP (optional):

```bash
cargo test -p fleet-driver --test acp_grok -- --ignored --nocapture
```
