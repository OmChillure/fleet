# Test standardization

Where a test belongs, now and when you add one. How to **run** tests is
[`testing.md`](./testing.md). Humans and MR videos are
[`human-testing.md`](./human-testing.md). What makes a test good enough
to merge: [`qa-rulebook.md`](./qa-rulebook.md).

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
| Units (no Postgres) | `just test` → workspace `--lib --exclude fleet-ui` + `cargo test -p fleet-driver` + host markdown / runners-view crates |
| Postgres integration | `just test-integration` → orch + auth + server + fleet-db |
| Lab + optional browser | `run-full-suite.sh` (browser only if `ENABLE_BROWSER_E2E=1`) |
| Human walkthrough | `./scripts/human-test.sh` — **HUMAN_TEST_SCOPE.md** only (not the full catalog every time) |
| Agent MR demo | `./scripts/human-test.sh record` on the **final merge tree**; scope scenes, not a random walk; then **review the film** for bugs |

There is **no GitLab runner**. Keep `.gitlab-ci.yml` as `when: never`; do not add jobs.
All tests must pass before a merge to main (`just test` + `just test-integration`).
All tests must be relevant.
If a test is not relevant, decide whether the test or the code should change.

## Fail closed

- Missing `DATABASE_URL` in a Postgres test **panics** with a one-line how-to.
- Missing UI/API when browser e2e or `record` is requested **fails**.
- Missing host `dx` **fails**. Kind UI is not a substitute. Do not
  WARN-and-continue in bootstrap / fleet-dev / every-test.
- Live identity is API **and** UI (`/health.git_sha` + `/fleet-git-sha.txt`).
  API-only was how an old UI went green. Compile-time bake only — never
  `kubectl set env FLEET_GIT_SHA`.
- `SKIP_*` on the suite is for operators who cannot run compose/kind/LLM, not
  to hide a red test. `FLEET_ALLOW_FOREIGN_STACK` is not a green path.
- Optional kind / git-ssh / Ollama / `:1234` skip only when the matching
  `ENABLE_*` is off. If the flag is on and the service is down, **fail**.
- Do not use `|| true` around `cargo test`.
- Do not print EVERY-TEST OK with caveats. Contract:
  `tests/browser/test_fail_closed_contract.py`.
- In-process orchestrator tests set `Config.orchestrator_public_url` to
  **this test's listener**. `API_PUBLIC_BASE` from repo-root `.env` is Kind
  `:8080` — if spawn re-reads it, process workers heartbeat at the foreign
  stack and the run goes `failed`.
- Live/e2e/every-test is valid only after the install script or
  `--prepare-kind` (+ host `dx`) baked `scripts/tree-id.sh` into the
  running stack. A green run on yesterday's images is not verification.
- The last pass before an MR is the **bookworm ship image**, not host
  `just test`. `Dockerfile` / `Dockerfile.ui` (`rust:1.88-bookworm`) is
  what VPS and Kind compile. Host-only green is a drift, not a gate.
- Do not create a recurring test-writer loop. Do not spawn one agent
  per catalog row on a timer. Harvest from `just test` /
  `scripts/harvest-runner-catalog.sh` / Playwright. A missing child id
  is not a relaunch.

## Runners (required, not implemented here)

Every runner behavior (nav, pair/install, cards, isolation, apply/ack,
control-plane runner, assignment, staging/updates, Cloud/Partners
placeholders) needs **three layers**: unit/regular, browser e2e, and
real-life guest e2e (VM per OS, install, heartbeat, jobs, updates).
Catalog: [`components/runners/testing-requirements.md`](./components/runners/testing-requirements.md).
How to run: [`components/runners/testing-procedures.md`](./components/runners/testing-procedures.md).
Do not ship a new runner behavior without the layers that apply.

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
7. All tests must be relevant. If a red or stale case is not locking the
   product anymore, decide whether the test or the code should change
   before you rewrite either.

## Closed on this branch

| Issue | Resolution |
| --- | --- |
| `run-host-e2e.py` under `scripts/` | `sandbox-lab/tests/test_host_e2e.py` (unittest) |
| `run-protocol-tests.sh` duplicates the suite | thin wrapper; prefer `run-full-suite.sh` |
| `test_host_services` skipped Postgres | `:5433` always fails if down; Ollama/:1234 fail when `ENABLE_*=1`, skip only when the flag is off |
| `fleet-ui` markdown tests never run | `fleet-chat-md` on `just test` |
| `fleet-db` had zero tests | `crates/fleet-db/tests/repos_integration.rs` |
| `fleet-auth` only persona units | password / JWT / cookie / config `#[cfg(test)]` |
| Server HTTP repeated orch tick matrix | one cookie BFF fixture+tick; orch keeps the matrix |
| Human/demo scripts unproven | films only after `/health.git_sha` matches HEAD; lab uses absolute `file://` |
| Live `:3030`/`:8080` stolen by Kind | `stack_identity.py` fail-closed; old `.webm`s are not proof |
| Host `dx` missing / Kind UI used as substitute | install installs `dx` or exits 1; bootstrap/every-test fail closed |
| `/health.git_sha` faked via `kubectl set env` | compile-time `option_env!`; UI `/fleet-git-sha.txt`; image label |
