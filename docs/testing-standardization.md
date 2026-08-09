# Test standardization

Where a test belongs, now and when you add one. How to **run** tests is
[`testing.md`](./testing.md). Humans and MR videos are
[`human-testing.md`](./human-testing.md).

## Rule

Test in the language of the thing under test. Fail closed. Do not add a second
framework.

| SUT | Put the test here | Not here |
| --- | --- | --- |
| Rust pure logic | `#[cfg(test)]` next to the code | Python, `tests/browser` |
| Rust public API, Postgres, binaries | `crates/<pkg>/tests/*.rs` | silent `eprintln!("skip"); return;`; a second HTTP stack in Python |
| Python workspace agent | `sandbox-lab/workspace/tests/` | `scripts/` |
| Install / compose / kind YAML | `sandbox-lab/tests/` | Rust |
| Dioxus chrome | `tests/browser/test_app_e2e.py` | asserting layout in Rust |
| New UI or operator surface | e2e **and** a `human-test.sh` step **and** a `record_demo.py` scene in the **same** change | “walkthrough later” |
| Live LLM / Grok ACP | `#[ignore]` | default green |
| Host-only protocol (HMAC, `fleet-wsexec`) | `sandbox-lab/tests/test_host_e2e.py` | `scripts/run-host-e2e.py` |

## Commands (must match `justfile` and `run-full-suite.sh`)

| Intent | Command |
| --- | --- |
| Units (no Postgres) | `just test` → workspace `--lib --exclude fleet-ui` + `cargo test -p fleet-driver` + host markdown crate |
| Postgres integration | `just test-integration` → orch + auth + server + fleet-db |
| Lab + optional browser | `run-full-suite.sh` (browser only if `ENABLE_BROWSER_E2E=1`) |
| Human walkthrough | `./scripts/human-test.sh` |
| Agent MR demo | `./scripts/human-test.sh record [scenes…]` |

There is **no GitLab runner**. Do not add `.gitlab-ci.yml`.

## Fail closed

- Missing `DATABASE_URL` in a Postgres test **panics** with a one-line how-to.
- Missing UI/API when browser e2e or `record` is requested **fails**.
- `SKIP_*` on the suite is for operators who cannot run compose/kind/LLM, not
  to hide a red test.
- `skipUnless` is only for optional kind / git-ssh / Ollama / `:1234`.
- Do not use `|| true` around `cargo test`.

## Future tests (checklist)

Before you write a test, pick the row in the table. Then:

1. If it is Rust behavior, write Rust. Do not add `tests/proofs/` or any
   Python HTTP clone. (The old `test/behavior-proofs` worktree stays unmerged.)
2. If it needs Postgres, put it in `crates/<pkg>/tests/` and use
   `require_database_url()`. Unique emails/UUIDs per case.
3. If you add a button, route, or install flag: unit/integration **and**
   walkthrough step **and** record scene, same commit.
4. Prefer Axum oneshot over spawning `fleet-server` for HTTP.
5. Prefer constructing config in-struct over `unsafe { env::set_var }`.
6. `fleet-ui` stays WASM: chrome in Playwright; pure helpers in a host crate
   (`fleet-chat-md` for markdown).

## Closed on this branch

| Issue | Resolution |
| --- | --- |
| `run-host-e2e.py` under `scripts/` | `sandbox-lab/tests/test_host_e2e.py` (unittest) |
| `run-protocol-tests.sh` duplicates the suite | thin wrapper; prefer `run-full-suite.sh` |
| `test_host_services` skipped Postgres | `:5433` fails if down; Ollama/:1234 still skip |
| `fleet-ui` markdown tests never run | `fleet-chat-md` on `just test` |
| `fleet-db` had zero tests | `crates/fleet-db/tests/repos_integration.rs` |
| `fleet-auth` only persona units | password / JWT / cookie / config `#[cfg(test)]` |
| Server HTTP repeated orch tick matrix | one cookie BFF fixture+tick; orch keeps the matrix |
| Human/demo scripts unproven | `record_demo.py` films core scenes; lab uses absolute `file://` |
