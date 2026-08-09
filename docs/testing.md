# Testing

How this repo is tested. Install is still one script
(`crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`)
and repo-root `.env`. This page only says **how to run tests**.

## Layers

| Layer | Where | Command | Skip? |
| --- | --- | --- | --- |
| Unit | `#[cfg(test)]` next to Rust code | `just test` → `cargo test --workspace --lib --exclude fleet-ui --locked` plus bin crates that hold unit tests | Never |
| Rust integration | `crates/*/tests/*.rs` | `just test-integration` (needs Postgres via `just db-up`) | **Fail** if `DATABASE_URL` is unset |
| Lab | `sandbox-lab/**/test_*.py` | `run-full-suite.sh` | `skipUnless` only for optional kind / git-ssh |
| Browser | `tests/browser/test_app_e2e.py` | suite stage (needs `:3030` + `:8080`) | Fail if the stack is down |
| Live LLM / Grok ACP | `#[ignore]` | `cargo test -p fleet-driver -- --ignored` | Ignored by default |

`fleet-ui` is a Dioxus/WASM app; it is excluded from `cargo test --workspace --lib`. Cover it with Playwright.

## Rules

- Rust behavior is tested in Rust. Python tests the Python workspace agent and compose/kind YAML.
- No silent `eprintln!("skip"); return;` in `crates/**/tests/*.rs`. Missing `DATABASE_URL` panics with a one-line how-to.
- Do not use `|| true` around `cargo test`.
- `SKIP_*` on `run-full-suite.sh` is for operators who cannot run compose/kind/LLM, not a way to hide failures.
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
