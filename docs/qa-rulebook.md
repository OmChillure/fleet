# QA rulebook

Enforceable rules for writing, reviewing, and shipping tests in this repo.

This file is the quality bar. It does not replace the runbook or the placement
map:

| Question | File |
| --- | --- |
| How do I run tests? | [`testing.md`](./testing.md) |
| Where does a new test live? | [`testing-standardization.md`](./testing-standardization.md) |
| How do humans walk the product and how do agents film an MR? | [`human-testing.md`](./human-testing.md) |
| Which runner behaviors need which layers? | [`components/runners/testing-requirements.md`](./components/runners/testing-requirements.md) |
| Is this test good enough to merge? | **This file** |
| Does each existing test meet these laws? | [`qa-test-matrix.md`](./qa-test-matrix.md) (unevaluated) |

Rules below are numbered so a review can cite them (`QA-R12`). A test that
violates a **must** rule is not mergeable. A **should** rule can be waived in
the MR with one sentence of why.

Sources that back each cluster are listed at the end. The repo-specific
fail-closed / identity / placement rules are already law in `AGENTS.md` and
the files above; this book restates them only so a reviewer has one checklist.

---

## 0. Laws

These are not guidelines.

1. **A test locks a live contract.** If the product can drop the behavior and
   users would notice, the test must fail. If nobody would notice, the test
   should not exist.
2. **Fail closed.** Missing Postgres, missing `dx`, missing UI/API, wrong
   tree-id, a down `ENABLE_*` service, or a foreign stack is a **fail**, not a
   skip, a warn, or `|| true`.
3. **Green means this tree.** Live/e2e/human-test/every-test is valid only
   when `GET /health` `git_sha` **and** UI `/fleet-git-sha.txt` equal
   `scripts/tree-id.sh`, after the sha-consuming bits were rebuilt.
4. **Tests are independent and deterministic.** Any order, any one test
   rerun, same result. Shared leftover state is a bug in the test.
5. **Test behavior through the public surface, not the private wiring.**
   A refactor that does not change the contract must not force a rewrite of
   the test body.
6. **Use the cheapest layer that can actually prove the claim.** Logic in
   Rust units. HTTP/SQL contracts in crate integration tests. Chrome in
   Playwright. Real guests for runner install/heartbeat/jobs. Do not add a
   second framework.
7. **Automation does not replace a person looking at the product.** User- or
   operator-visible changes still get a `HUMAN_TEST_SCOPE.md` walk and, on
   the final merge tree, a filmed demo that is reviewed for bugs.

---

## 1. What a good test is

A good test has high **fidelity** (it fails when the product is broken) and
high **resilience** (it does not fail when the product is not broken). Those
two properties pull in opposite directions; the rest of this book is how to
hold both.

### QA-R1. One behavior, one name, one reason to fail

**Must.** A test name states the behavior and the condition, not the method
under test.

```
signup_201_no_session_cookies          # good
signup_rejects_bad_email               # good
test_auth                              # bad — what about auth?
should_work                            # bad
```

When it fails, a reader who has never opened the file should know which
contract broke. If you need an `if` to decide which assertion to run, you
have two tests.

Do not map tests 1:1 to public methods. Separate behaviors get separate
tests. Verifying every effect of a method in one function is Meszaros's
**Eager Test** smell. At e2e scope, one *use case* (and its important
error class) is the unit — not every field on the page.

### QA-R2. Four phases, visible on the page

**Must.** Setup → exercise → verify → teardown (Meszaros Four-Phase Test;
Bill Wake Arrange-Act-Assert). A reader must be able to point at the one
line that is the act.

Do not hide the act inside a helper named `run()` that also asserts. Helpers
build fixtures and read responses; the test function asserts the contract.

Multiple interleaved act–assert blocks, or more than one exercise phase,
mean split the test. It is no longer a single-condition test, and the
focus object is hard to see.

### QA-R3. Complete and concise

**Must.** The test body contains every fact a reader needs to understand the
expected result, and nothing else. Magic numbers (`assert_eq!(result, 5)`)
without the inputs that produce 5 are incomplete. A 80-line fixture dump
for a one-line contract is not concise.

Prefer DAMP over DRY in tests (Google SWE book). Extract a helper when
duplication hides the behavior; keep duplication when extraction would hide
the inputs.

### QA-R4. No logic in the test

**Must.** No production-like `if` / `for` / `match` that computes the
expected value. Expected values are literals or trivial constructors. A
loop that builds N unique emails is fixture setup, not an oracle. A loop
that re-implements the SUT to compute the expected output is a tautology.

Google's rule: tests themselves have no tests, so they must be obvious on
inspection. Control flow in a test is a smell; table-driven cases with
literal expected values are the replacement.

### QA-R5. FIRST

**Must.**

| Letter | Meaning here |
| --- | --- |
| Fast | Units run on every save. Integration is the next gate, not the first. |
| Isolated | No order dependence. Unique emails / UUIDs per case (`auth.{uuid}@example.test`). |
| Repeatable | Same result on this machine. The deployer re-proves the bookworm ship image. |
| Self-validating | Pass or fail. No log-reading to decide. |
| Timely | Written in the same change as the behavior. |

---

## 2. What to test, and what not to

### QA-R6. Beyoncé rule

**Must.** If you liked it, put a test on it. If you want to be confident the
system keeps a behavior — correctness, authz, isolation, failure handling,
the install path, the pair token, the cookie — there is an automated test
for that behavior. Infrastructure refactors that pass the suite but break
the product are the product team's miss, not the refactor's.

### QA-R7. Lock the contract, not the wiring

**Must.** Call the system the way a user of that layer would:

| Layer | Public surface |
| --- | --- |
| Pure Rust | The function / type's public API |
| Crate HTTP | Axum oneshot against the real router |
| Repo / SQL | Real Postgres via `require_database_url()` |
| UI chrome | What the user sees and clicks |
| Runner guest | Install command this plane serves, then heartbeat / job |

Do **not** lock:

- Private helper names, serialization order of an internal struct, call
  order into a mock unless that order **is** the contract (for example HMAC
  headers, cookie flags, workspace isolation).
- CSS class names, DOM depth, Dioxus internal state.
- Error-string copy that is not a published API. Status code + error code
  / stable field is the contract. A substring check on `"8"` for a minimum
  password length is acceptable only because the number is the contract;
  a check that the sentence is worded a certain way is not.
- Product fiction that the UI no longer offers.
- Tautologies: `assert_eq!(2 + 2, 4)`, `assert!(true)`,
  `assert_eq!(config.field, config.field)`, a mock scripted to return X
  then asserted to be X.

### QA-R8. State over interactions

**Should.** Assert what is true after the act (status, body field, cookie
presence, DB row, UI text, log line that the server actually handled the
request). Asserting that `mock.sort()` was called does not prove sorting
works.

Interaction tests are allowed when **how** is the contract: HMAC signed,
token not logged, workspace A cannot read workspace B's rows, the worker
heartbeats at **this** test's listener rather than `API_PUBLIC_BASE`.

### QA-R9. Do not test what you do not own

**Must.** Do not assert on GitHub.com, a public weather API, or a third-party
cookie banner. Stub the network at the boundary you own, or run a fake that
the owner of that boundary maintains. Playwright: route-fulfill the
third-party URL; do not `goto` it.

Live LLM / Grok ACP stays `#[ignore]` / `LIVE_SKIPPED` until a human
enables it. Default green never depends on a model.

### QA-R10. Relevance is a merge gate

**Must.** Already in `AGENTS.md`:

- A test locks a live contract, not error-string copy, a removed product
  fiction, or a tautology.
- If a test is not relevant, decide whether the **test** or the **code**
  should change.
- Do not keep a stale assertion to stay green.
- Do not delete a live contract because the code fails it.

A red test that still describes the product is a bug in the code. A green
test that describes a page we removed is a bug in the test.

An empty host list, an empty session list, or “no Kill” on an empty or
terminal list is not a real check — the interesting path never ran. A
human-surface contract is an inventory lock (which scenes exist), not a
snapshot of today’s scene list, and it does not replace a person looking
at the product.

---

## 3. Assertions

### QA-R11. Assert the thing that would be wrong

**Must.** Narrow assertions. `assert_eq!(out.status, CREATED)` plus the one
field that is the point of the test. Do not deep-equal a whole JSON blob
that includes timestamps, request ids, or unordered maps unless those
fields are the contract.

Broad equality (`JsonEquals` the entire response, screenshot the entire
page, snapshot a 200-line struct) is how tests become brittle. Google
TotT: a test written `IsSquare(output)` survives a size change; a test
written `width == 42 && length == 42` does not, when size was never the
point.

### QA-R12. Failure messages name the case

**Must.** Every non-obvious assertion includes the input or the body:

```rust
assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
assert!(
    out.cookie(ACCESS_COOKIE).is_none(),
    "signup must not set {ACCESS_COOKIE}: {:?}",
    out.set_cookies()
);
```

A CI log that only says `left: 401, right: 201` without the email, the
path, or the body wastes the next hour.

### QA-R13. Browser assertions wait for the product, not the clock

**Must.** Playwright web-first assertions (`expect(locator).to_be_visible()`)
retry. `assert locator.is_visible()` does not. Never `time.sleep` to "let
the page settle." Never `expect(await locator.is_visible()).to_be(True)`.

Locate by role and accessible name, then label, then test id. CSS / XPath
/ nth-child is a last resort and needs a comment saying why.

### QA-R14. Cross the UI claim with a server fact

**Should** for any browser test that clicks a mutating control. The UI
saying "saved" is not enough. The matching `fleet-server` log line
(`method=` + `uri=` + `status=`) or a follow-up GET must show the write
happened. Human-test film review already requires this; automated e2e
should too.

### QA-R15. Think like a mutant

**Should.** After you write the assertion, ask: if I inverted this
conditional, deleted this line, or swapped these two arguments, would
this test fail? If not, you executed the line (coverage) without detecting
a fault. Coverage is a map of gaps, not a quality score. Do not add a test
to move a percentage.

---

## 4. Isolation, time, and hermeticity

### QA-R16. Fresh data per case

**Must.** Unique emails / UUIDs / host names per test
(`srv.{uuid}@example.test`). Do not reuse `user@example.com`. Do not
depend on row 1 of a shared table. Do not require a particular run order.

Postgres tests share a migrated database. Isolation is by unique keys, not
by assuming an empty DB. If a test needs a clean project, it creates one.

### QA-R17. Construct config; do not mutate process env

**Must.** Prefer in-struct `Config { .. }` over `unsafe { env::set_var }`.
In-process orchestrator tests set `orchestrator_public_url` to **this
test's listener**. Re-reading repo-root `.env` `API_PUBLIC_BASE` sends
heartbeats at the Kind stack and the run goes `failed` for the wrong
reason.

### QA-R18. Control clocks, time, and randomness

**Must** when the SUT uses them. Inject a clock. Do not `sleep` to wait
for "90s offline." Advance the clock. Do not assert `now` in a response
without a tolerance that is itself a contract. Seeds for any RNG are
fixed.

### QA-R19. Hermetic by default

**Must.** A unit or crate integration test does not call production, does
not need the UI, and does not need Kind. Network is either in-process
(oneshot) or a local listener the test started.

Live guests, live OAuth, and live LLM are the exceptions, gated by
`ENABLE_*` / `#[ignore]`, and they **fail** when the flag is on and the
dependency is down.

### QA-R20. Parallel-safe

**Should.** Tests that cannot run next to themselves (fixed port, fixed
temp path, fixed email) are broken. Use ephemeral ports, `tempdir`, and
UUIDs. Cargo and pytest will parallelize; do not make them single-thread
to hide a collision.

`cargo test` runs in parallel by default. Two tests that write
`test-output.txt` interfere; each test writes a different file. Two tests
that `env::set_var` the same key interfere — that is also why QA-R17
bans process-env mutation. Playwright parallel tests derive unique
backend ids from the test id and write files via the test output path;
they must not share a mutable module, cookie jar, or backend row.

---

## 5. Pick the right layer

Cohn / Fowler: many small fast tests, some mid-layer tests, very few
broad-stack tests. Google SWE: roughly 80% narrow / 15% medium / 5%
end-to-end as a starting mix, not a quota (they also cite 70/20/10 as a
first guess). Those ratios are a shape, not a gate. Fowler's two rules
that survive the naming debates: **write tests of different granularity**,
and **the higher you go, the fewer you write**.

Google classifies by **size** (what the test is allowed to do), which
infrastructure can enforce, not only by “unit / integration / e2e”:

| Size | Allowed | Forbidden |
| --- | --- | --- |
| Small | Single process, often single thread | Server+client, third-party DB, `sleep`, network, disk |
| Medium | Multiple processes / threads, localhost only | Remote network |
| Large | Multi-machine, full system | — reserved for journeys smaller tests cannot evaluate |

Write the **smallest** test that can evaluate the behavior. If two units
fail to integrate, write a focused integration test, not an e2e test.
When a high-level test fails, **replicate the failure as a unit test
before fixing it** — then the e2e stays a second line of defense.

This repo's mapping:

| Claim you need to prove | Layer | Where |
| --- | --- | --- |
| Pure function, parser, policy, cookie flags, JWT, markdown | Unit | `#[cfg(test)]` next to the code; host crates for UI helpers (`fleet-chat-md`, `fleet-runners-view`) |
| HTTP status/body, authz, workspace isolation, repo writes | Rust integration | `crates/<pkg>/tests/*.rs`, Axum oneshot, `require_database_url()` |
| Install / compose / kind YAML, host protocol | Lab Python | `sandbox-lab/tests/` |
| User can click it and the chrome is right | Browser e2e | `tests/browser/test_*.py` against **this tree** |
| A real OS can pair, heartbeat, take a job, update | Guest e2e | lab VM matrix; missing guest + flag on = fail |
| The screen still makes sense to a person | Human + film | `human-test.sh` + `record_demo.py` on the final merge tree |

### QA-R21. Do not promote a unit claim to e2e

**Must.** "Password must be 8 characters" is a Rust test. "The signup
button is clickable and lands on login" is browser e2e. Repeating the
password-length matrix in Playwright is how the suite becomes an ice-cream
cone (lots of slow UI tests, few units).

Google TotT on e2e: one end-to-end test per important use case and per
important error class; keep the total count low; budget maintenance
(they estimate on the order of a week a quarter per e2e test when
dependencies drift).

### QA-R22. Do not demote a system claim to a mock script

**Must.** Workspace isolation, cookie BFF, pair → lease → job, OAuth
state-nonce one-time use — these are integration tests against real
Postgres and the real router. A mock that returns `{"ok": true}` does not
lock those contracts.

### QA-R23. Language of the SUT

**Must.** Rust behavior in Rust. Python workspace agent and compose/kind
YAML in Python. Dioxus chrome in Playwright. No `tests/proofs/` Python
HTTP clone of the API. No second framework.

### QA-R24. New surface, three artifacts, same change

**Must** when you add a user- or operator-visible path:

1. the automated test at the right layer(s),
2. a `scripts/human-test.sh` step,
3. a `tests/browser/record_demo.py` scene,

and `python3 tests/browser/surface_scope.py --write` so `HUMAN_TEST_SCOPE.md`
lists every surface the diff can break. "Walkthrough later" is not a plan.

Runner behaviors additionally need the three layers in
`components/runners/testing-requirements.md` (unit, browser e2e, real
guest). Placeholders (Cloud, Partners) still need layers 1–2 so they
cannot silently become assignable.

---

## 6. Fail closed

### QA-R25. Missing dependency is a fail

**Must.**

| Situation | Required outcome |
| --- | --- |
| Postgres integration, `DATABASE_URL` unset | `panic!` with a one-line how-to. Never `eprintln!("skip"); return;` |
| Browser e2e / `record` requested, UI or API down | Fail |
| Host `dx` missing | Fail. Kind UI is not a substitute |
| `ENABLE_*=1` and that service is down | Fail |
| `ENABLE_*=0` | Skip only that optional bucket |
| `|| true` around `cargo test` | Forbidden |
| `EVERY-TEST OK` with caveats | Forbidden |
| `FLEET_ALLOW_FOREIGN_STACK` | Not a green path |
| `.gitlab-ci.yml` grows a job / include / runner | Forbidden (`when: never` only) |

`SKIP_*` on `run-full-suite.sh` is for an operator who cannot run
compose/kind/LLM on that machine. It is not a way to hide a red test on
a machine that can run it.

### QA-R26. Identity is compile-time and dual-sided

**Must.** `/health.git_sha` is baked at compile time
(`option_env!("FLEET_GIT_SHA")`). Never `kubectl set env FLEET_GIT_SHA`.
API-only identity is how an old UI went green — require the UI file too.
`:local` images skip rebuild only when label `fleet.git_sha` matches
`scripts/tree-id.sh`. After any code change, rebuild before treating
live/e2e/human-test as valid.

### QA-R27. The deployer proves the ship image

**Must.** Host `just test` can be green while
`Dockerfile` / `Dockerfile.ui` (`rust:1.88-bookworm`) does not compile
(OpenSSL, glibc, sqlx). The author does not run that `docker build`.
The deployer does, with `PROVE_ONLY=1 DEPLOY_SHA=<commit>` in
`deploy/studio/scripts/deploy.sh`, before the SHA is mergeable. That
same pass builds the three ship runner platforms and registers their
hashes. `./scripts/run-every-test.sh --prepare-kind` is the lab bake
when a live or browser run needs it, not the ship gate.

### QA-R28. Ignored means ignored on purpose

**Must.** `#[ignore]` / `LIVE_SKIPPED` is a dated, named hole (live Grok
ACP; listed guest OS/arch that cannot boot here), not a parking lot for a
test you could not get green. A skip that hides a broken path is a fail.

---

## 7. Flakiness

A flaky test is a false alarm. False alarms train people to ignore red.
Google's large-scale finding: the only important results come from tests
that fail deterministically. A test that is red 2% of the time has no
signal. Fowler: if production code, tests, and environment are unchanged,
the result must not change; a test that passes and fails with no
noticeable change is useless as a bug detector and spreads.

Empirical work (Luo et al., FSE 2014) found the dominant flake causes
are async wait, concurrency, and test-order / shared-state coupling
(~78%), plus resource leaks, network, time, I/O, randomness, floating
point, and unordered collections. Do not ignore or delete a flake to
go green — find the shared state and make it unique, or inject the
clock.

### QA-R29. Flake is a P0 on the test, not a retry config

**Must.** Do not hide flake with a rerun budget. Fix or delete. Common
causes, banned here:

- `sleep` / `waitForTimeout` / fixed timeout instead of a condition
- Shared email, port, temp path, cookie jar, or `test-output.txt`
- Reading a page before the write landed (no wait, no log cross-check)
- Timezone / locale / `now()` in an equality — inject a clock
  (Playwright clock API for `Date` / timers / `performance`)
- Unseeded `Math.random` / unfixed RNG
- Unordered collections compared as lists
- Hitting Kind `:8080` that is yesterday's image
- Background ticker racing the test (control it or assert eventually with
  a bounded, explicit condition)
- Screenshot diffs of a whole page
- Calling a third-party server instead of the Network API / a local fake

### QA-R30. Isolation over cleanup

**Must.** Prefer a fresh browser context / fresh user / fresh workspace
over "delete what the last test created." Cleanup is easy to forget and
some things (visited links, sequence numbers) cannot be cleaned. Playwright
already gives you a context per test; do not opt out to share a login
unless the shared state is a read-only storage-state and each test still
creates its own rows.

---

## 8. Naming, shape, and reviewability

### QA-R31. Name the behavior, not the mechanism

**Must.** `signup_201_no_session_cookies` beats `test_signup_handler`.
Table rows have names, not indexes (`case 3` is not a name).

### QA-R32. The test is the spec

**Should.** Someone new to the crate should be able to learn the public
contract by reading the tests. If the tests only make sense after reading
the implementation, they are testing the implementation.

### QA-R33. Review the test as hard as the code

**Must.** A change without tests for a new behavior is incomplete (Google:
reviewers are expected to block on missing tests). A change that "fixes"
a test by loosening the assertion to match a bug is a regression. Ask:

1. What contract does this lock?
2. Would a user notice if it failed?
3. Would a harmless refactor force a rewrite?
4. Can I run this test alone and get the same result?
5. If I broke the product on purpose, would this fail?

---

## 9. Human QA that automation cannot replace

Automated tests show the presence of bugs and the preservation of
contracts. They cannot judge whether the screen makes sense.

Automate work that is tedious, repetitive, error-prone, acceptance-related,
or high-regression — especially multi-OS matrices and new or heavily
changed code. Do **not** automate a one-off, a case that is complicated
for little time saved, or a render-style check a person should see once.
Deciding what stays manual is as important as deciding what to test.

### QA-R34. Walk the scope, not the catalog

**Must.** Humans run `./scripts/human-test.sh` against
`HUMAN_TEST_SCOPE.md` (regenerated from the diff). `guide-all` is a first
product tour only. Throwaway `*@example.test` accounts. No real emails,
tokens, or customer data.

### QA-R35. Film the final tree, then review the film

**Must** for user- or operator-visible MRs. Identity matches
`scripts/tree-id.sh`. The recorder **uses** each feature (click, create,
cancel, save, tick, invite, pair) and crosses each click with the server
log. Opening a heading is not a review.

Fail the film review when a control is present but unusable, when the UI
claims success the log never saw, or when the clip was shot before later
edits. Never commit `.webm`s.

### QA-R36. Exploratory still exists

**Should.** After the scripted walk, spend a few minutes trying to break
the new thing: back button, second tab, expired cookie, empty state, huge
paste, the other workspace. Exploratory findings that are contracts become
automated tests in the same MR or a follow-up filed before merge.

---

## 10. Anti-patterns (reject in review)

| Smell | Why it is bad | Do this instead |
| --- | --- | --- |
| Tautology / no assertion | Coverage without fidelity | Assert a user-visible contract |
| Empty-list “success” | The interesting path never ran | Create the row, then assert |
| Eager Test (every effect in one fn) | Many reasons to fail; hard to name | One behavior per test |
| Error-string snapshot | Brittle copy, not a contract | Status + stable error code / field |
| Mock-is-the-SUT | You tested the stub | Oneshot + Postgres, or a maintained fake |
| Private-field peek | Breaks on every refactor | Public API / HTTP / UI |
| `sleep(2)` | Flake or slowness | Condition, clock inject, web-first expect |
| `eprintln!("skip"); return;` | Green on a missing DB | `require_database_url()` panic |
| `\|\| true` | Green on a red suite | Fail the job |
| Shared `user@example.com` | Order dependence | `unique_email()` |
| Whole-page screenshot as the spec | Layout noise | Role locator + one outcome + server log |
| Second Python HTTP stack | Drift from Axum | `crates/<pkg>/tests` |
| Recurring test-writer agent | Harvest, don't generate | `just test` / harvest scripts / Playwright |
| Film of yesterday's image | Wrong tree | Rebuild, re-film, review |
| Loosening the assert to merge | Hides the bug | Fix the code or delete a dead contract |

Meszaros names that map onto the table: **Assertion Roulette** (many
asserts, no messages), **Obscure Test** (cannot see the act), **Eager
Test** (every effect of a method in one test), **Conditional Test** (an
`if` that can pass without asserting), **Erratic Test** (flake),
**Fragile Test** (sensitive equality / layout), **Slow Tests** (e2e used
as units).

---

## 11. Reviewer checklist

Copy into the MR if useful. Cite the rule id.

**Placement and layer**

- [ ] Right language, right directory (`QA-R23`, standardization table)
- [ ] Cheapest layer that proves the claim (`QA-R21`, `QA-R22`)
- [ ] New UI/operator surface has e2e + human step + record scene (`QA-R24`)

**Contract**

- [ ] Name states the behavior (`QA-R1`)
- [ ] Asserts state a user of this layer would care about (`QA-R7`, `QA-R8`)
- [ ] Not a tautology, empty-list “success”, dead fiction, or copy-lock (`QA-R10`)
- [ ] One behavior (not an Eager Test of every effect) (`QA-R1`)
- [ ] Breaking the product on purpose would fail this test (`QA-R15`)

**Shape**

- [ ] Four phases visible; no hidden asserts in helpers (`QA-R2`)
- [ ] No oracle logic (`QA-R4`)
- [ ] Failure message includes input or body (`QA-R12`)

**Hermetic / fail-closed**

- [ ] Unique fixtures; no shared identity (`QA-R16`, `QA-R20`)
- [ ] Missing dep panics or fails, never skips (`QA-R25`)
- [ ] No `sleep`, no process-env mutation (`QA-R13`, `QA-R17`, `QA-R29`)
- [ ] Live/e2e only after identity matches this tree (`QA-R26`)

**Before merge**

- [ ] `just test` and `just test-integration` green on this tree
- [ ] Deployer ship proof for this SHA (`QA-R27`), not an author `docker build`
- [ ] `HUMAN_TEST_SCOPE.md` regenerated; film reviewed if required (`QA-R34`–`R35`)
- [ ] No red hidden by ignore / `|| true` / foreign stack (`QA-R28`)

---

## 12. Worked examples from this tree

**Good — contract, public HTTP, unique fixture, fail-closed**
(`crates/fleet-auth/tests/auth_http.rs`):

```rust
#[tokio::test]
async fn signup_201_no_session_cookies() {
    let app = app().await;
    let email = unique_email();
    let out = signup(&app, &email, TEST_PASSWORD).await;

    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert!(
        out.cookie(ACCESS_COOKIE).is_none(),
        "signup must not set {ACCESS_COOKIE}: {:?}",
        out.set_cookies()
    );
    let me = call(&app, "GET", "/api/auth/me", None, None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED, "signup is not a login");
}
```

Why it passes this book: one behavior, public router, unique email, status
+ cookie + follow-up GET (state, not a mock interaction), panic if
`DATABASE_URL` is missing (`common::require_database_url`).

**Good — layer choice.** Password policy and JWT live in `#[cfg(test)]`
next to `fleet-auth`. Cookie BFF + tick matrix live in `fleet-server` /
`fleet-orchestrator` integration tests. Sidebar chrome lives in
Playwright. Pair → heartbeat → job on a real VM lives in guest e2e.

**Bad — would reject.** A Playwright test that `sleep(3)`s, clicks
`.btn.btn-primary:nth-child(2)`, and asserts the page HTML contains
`"Success"` with no server-log cross-check, shared `demo@example.com`,
and `|| true` in the wrapper script.

---

## 13. Sources

Primary sources this book draws on. Quotes and numbers above track these
pages, not secondary blogs.

| Source | What we took |
| --- | --- |
| [Software Engineering at Google, ch. 11 — Testing Overview](https://abseil.io/resources/swe-book/html/ch11.html) | Beyoncé rule; small/medium/large size (what a test may do); 80/15/5 and 70/20/10 as a first guess, not a gate; no release with red tests |
| [Software Engineering at Google, ch. 12 — Unit Testing](https://abseil.io/resources/swe-book/html/ch12.html) | Unchanging tests; public APIs; state not interactions; complete and concise; DAMP not DRY; no logic in tests |
| [Google Testing on the Toilet — Test Behavior, Not Implementation](https://testing.googleblog.com/2013/08/testing-on-toilet-test-behavior-not.html) | Tests independent of private wiring |
| [Google Testing on the Toilet — Testing State vs. Testing Interactions](https://testing.googleblog.com/2013/03/testing-on-toilet-testing-state-vs.html) | Prefer state; interactions only when *how* is the contract |
| [Google Testing on the Toilet — Test Behaviors, Not Methods](https://testing.googleblog.com/2014/04/testing-on-toilet-test-behaviors-not.html) | Do not map tests 1:1 to methods |
| [Google Testing on the Toilet — Writing Descriptive Test Names](https://testing.googleblog.com/2014/10/testing-on-toilet-writing-descriptive.html) | Name the scenario and the expected outcome |
| [Google Testing on the Toilet — What Makes a Good End-to-End Test?](https://testing.googleblog.com/2016/09/testing-on-toilet-what-makes-good-end.html) | One e2e per important use case / error class; keep the count low |
| [Google Testing Blog — Just Say No to More End-to-End Tests](https://testing.googleblog.com/2015/04/just-say-no-to-more-end-to-end-tests.html) | Replicate a high-level failure as a unit test before fixing |
| [Google Testing Blog — Code coverage best practices (2020)](https://testing.googleblog.com/2020/08/) | Coverage finds gaps, not quality; mutation testing for assertion strength; do not chase 100% |
| [Google Testing Blog — Where do our flakes come from? (2020)](https://testing.googleblog.com/2020/12/where-do-our-flakes-come-from.html) | Hermetic environments; async-wait is a common flake source |
| [Martin Fowler — TestPyramid](https://martinfowler.com/bliki/TestPyramid.html) | Many unit tests, few broad-stack UI tests; record-playback is a bad default |
| [Martin Fowler — Eradicating Non-Determinism in Tests](https://martinfowler.com/articles/nonDeterminism.html) | Unchanged code + tests + env ⇒ unchanged result; flake spreads |
| [Ham Vocke / Fowler — The Practical Test Pyramid](https://martinfowler.com/articles/practical-test-pyramid.html) | Different granularity; fewer tests as you go up; avoid the ice-cream cone; exploratory still exists |
| [Mike Cohn — Succeeding with Agile](https://www.mountaingoatsoftware.com/blog/the-forgotten-layer-of-the-test-automation-pyramid) | Original pyramid (unit / service / UI) |
| [Kent Beck — Test Desiderata](https://medium.com/@kentbeck_7670/test-desiderata-94150638a4b3) | Isolated, deterministic, specific, behavioral, inspiring; reject tests that only reiterate the source |
| [Gerard Meszaros — xUnit Test Patterns](http://xunitpatterns.com/) | Four-phase test; Isolated / Repeatable principles; smells (Eager Test, Assertion Roulette, Conditional Test, Erratic Test, Fragile Test) |
| [Bill Wake — Arrange-Act-Assert](https://xp123.com/articles/3a-arrange-act-assert/) | Three-phase naming |
| [Playwright — Best practices](https://playwright.dev/docs/best-practices) | Test user-visible behavior; isolate; locators by role; web-first assertions; do not test third parties |
| [Playwright — Isolation](https://playwright.dev/docs/browser-contexts) / [Parallelism](https://playwright.dev/docs/test-parallel) / [Clock](https://playwright.dev/docs/clock) | Fresh context; unique test ids / output paths; no `waitForTimeout`; inject the clock |
| [The Rust Book — Writing Automated Tests](https://doc.rust-lang.org/book/ch11-00-testing.html) / [Running Tests](https://doc.rust-lang.org/book/ch11-02-running-tests.html) | Setup / run / assert; units next to code; integration against the public API; parallel by default; `#[ignore]` is opt-in, not green |
| [Azure Well-Architected — Testing](https://learn.microsoft.com/en-us/azure/well-architected/operational-excellence/testing) | Automate the high-regression matrix; keep exploratory / UX / unknown security risks human |
| [Microsoft — When To Automate (2005, archived)](https://learn.microsoft.com/en-us/archive/blogs/micahel/when-to-automate) | Skip one-offs and cases that cost more to automate than they save |
| [Luo et al. — An Empirical Analysis of Flaky Tests (FSE 2014)](https://mir.cs.illinois.edu/farah/presentations/fse14_presentation.pdf) | ~78% of flakes from async wait, concurrency, or order / shared state |
| [PIT mutation testing](https://pitest.org/) | Coverage measures execution; mutation measures whether tests detect faults |
| This repo | `AGENTS.md`, `docs/testing.md`, `docs/testing-standardization.md`, `docs/human-testing.md`, `tests/browser/test_fail_closed_contract.py` |

FIRST (fast, isolated, repeatable, self-validating, timely) is the
Ottinger / Langr compilation of properties that also appear, under other
names, in Beck, Fowler, Meszaros, and the SWE book. Those authors do not
present FIRST as their own checklist.

**Caveats from the research pass.** “Tautology” / “error-string copy” /
fail-closed merge policy are this repo’s language, not Google’s or
Microsoft’s. Official Playwright and `cargo test` still document retries
and `#[ignore]`; this book forbids treating those as a green path.
Pyramid and trophy ratios are heuristics; Fowler notes exceptions when
high-level tests are themselves fast and cheap. The SWE book does not
use “coverage as vanity” or discuss mutation testing — that contrast is
later Google Testing Blog and PIT. Meszaros relaxes “one condition per
test” for large customer tests; we keep it strict for units and crate
integration, and use “one use case” at e2e.

---

## 14. How to use this file

- **Writing a test:** pick the layer (standardization table), then walk
  sections 1–6 until the test would pass the checklist.
- **Reviewing a test:** use section 11; cite `QA-R#` in the comment.
- **Changing this book:** if a rule conflicts with `AGENTS.md` or the
  fail-closed contract tests, those files win until this book is updated
  in the same change. Do not add a second testing philosophy.
