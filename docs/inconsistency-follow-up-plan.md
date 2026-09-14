# Inconsistency follow-up plan

Source of truth for **what to do next**: the resolved table in
[`INCONSISTENCY-DECISIONS.md`](../INCONSISTENCY-DECISIONS.md).
This file is the implementation plan for work **not** finished in the first
consistency MR.

Do not re-open a decision that already has a checked box unless product
intent changed. Ops notes that are not code (VPS `.env`) are listed first.

---

## Already landed (this MR)

First pass was docs/config. The follow-up work below is now on the same
branch (not split into more MRs).

| Follow-up | Status |
| --- | --- |
| O1 VPS CORS + Caddy www→apex | Done on us-vmmedium (2026-09-14) |
| Isolation two axes | Done (`agent_runtime` / `workspace_location`) |
| Workspace persona vs roster specialist | Done |
| Instance cap 128 / workspace default 16 | Done (`instance_settings` + Admin save) |
| One migrator | Done (orch SQL copied into `fleet-db`; orch `migrate` calls `fleet_db::migrate`) |
| Provider registry | Done (DeepSeek not registered) |
| Lab leftover | Install moved to `scripts/`; old path is a wrapper; crate kept for tests |
| Zenoh mid-run share | First slice: `fleet/*/run/*/share/**` + team_chat outbox |

Docs, config, and contract copy from the first pass:

| Decision | What this MR did |
| --- | --- |
| #1 | `.env.example` → `fleet@127.0.0.1:5433/fleet` |
| #2 | `just` / harvest / testing.md use `.env` only |
| #3 | Kind remap `18080`/`13030` documented; `just` reads `PORT` / `FLEET_UI_PORT` |
| #4 | Loopback `API_PUBLIC_BASE` / `UI_PUBLIC_BASE` / `CORS_ORIGINS` follow listen ports; public hostnames stay |
| #5 | Two public bases documented; unset worker base = `API_PUBLIC_BASE` |
| #6 (repo) | Caddy `www` → apex; studio `env.example` CORS is apex-only |
| #7–#11, #17–#21, #24–#26, #30–#35 | README / crate READMEs / OpenAPI / UI map / names match live product |
| #12, #27, #29 (docs) | Architecture says HTTP tick assigns; Zenoh is the mid-run fabric |
| #28 (docs) | Product guest is `fleet-driver`; lab is leftover |

Unit tests added: loopback public-base rewrite, CORS rewrite.

---

## Ops (no MR)

### O1 — VPS apex-only CORS

**Depends on:** this MR’s Caddyfile shipping to us-vmmedium.

`/home/agent-fleet/stack/.env` still has
`CORS_ORIGINS=https://agentfleet.studio,https://www.agentfleet.studio`.
Deploy never overwrites that file.

1. After Caddy with the www redirect is live, set
   `CORS_ORIGINS=https://agentfleet.studio`.
2. Recreate the `api` container (`FLEET_GIT_SHA` from `DEPLOYED_SHA`).
3. Confirm `www` 301/302 → apex and login cookies stay on apex.

Do **not** rotate pair tokens or restage worker dist as part of this.

---

## Follow-up MRs (do in this order)

Each row is one merge request unless a later note says it can merge with
its neighbor. Keep ship-image + `HUMAN_TEST_SCOPE.md` per AGENTS.md.

### MR 1 — Isolation: two axes, names follow function (#13)

**Why:** Settings floor (`process`/`container`/`vm`) and host
`native`/`sandboxed` plus `on_pc`/`in_container` share the word “isolation”
and 409 if you send the other enum. Kind/container is already assignable.

**Product rule (from the decision sheet):** two axes because

1. the **agent** can run on the host (TUI) or in a container/VM (API tokens);
2. the **repo** can live on the host or in a container/VM.

Name the axes for those jobs. Do not collapse them to one enum.

**Do**

- Pick two field names and use them in API, DB comments, Settings, host
  README. Suggested (change if a better pair is obvious in the UI):
  - **agent runtime:** host process (TUI) vs container/VM
  - **workspace location:** repo on host vs repo in container/VM
- Map today’s wires: `min_isolation` → workspace-location floor;
  `run_mode` / `run_host`/`run_container` → agent runtime;
  host `isolation` `native`/`sandboxed` either becomes a derived label or
  is renamed so it is not “isolation”.
- Delete “not assignable / no k8s in v1” in
  `015_runner_isolation.sql` comments and
  `crates/fleet-db/src/models/worker_host.rs`.
- PATCH tests: each axis accepts its own vocabulary and rejects the other
  with a clear 400/409.

**Do not:** change Kind `RUN_HOST`/`RUN_CONTAINER` defaults in this MR
unless a test requires it.

**Accept:** Settings and host create/patch cannot 409 by sending a valid
value from the other axis. Assignable-host tests still include
sandboxed+container.

**Touches:** `fleet-db` models + repos, Settings UI, `worker_hosts` HTTP,
host README, `run-on-pc-vs-container.md`.

---

### MR 2 — Workspace persona vs roster specialist (#14)

**Why:** `/personas` and `/fleet/personas` / `list_agent_personas` are
different JSON under one noun.

**Do**

- Call workspace rows **workspace personas** (CRUD, seed 8 starters — #15
  already decided).
- Call agency-agents rows **roster specialists**.
- Agents UI: two sections, two DTOs. No shared `Persona` type that lies.
- `list_agent_personas` returns specialists (and may list workspace
  personas as a separate array or prefixed slugs — pick one and test it).
- Docs: `docs/components/personas/`, Agents page copy.

**Accept:** a client cannot confuse the two payloads. Chat still launches
with `worker_profile` slugs.

**Touches:** `fleet-auth` personas, `fleet-orchestrator/src/personas.rs`,
`chat_tools.rs`, `crates/fleet-ui/src/pages/agents/`.

---

### MR 3 — Instance cap 128, workspace default 16 (#22)

**Why:** `ensure_project` hardcodes 3; Settings default is 16; docs once
said 256/1000. Decision: instance-wide hard cap default **128**,
per-workspace live count default **16** (may grow to 1024+ later).

**Do**

- Instance admin setting `max_agents_cap` (or equivalent) default 128,
  persisted (new `fleet-db` migration). Admin UI on `/admin`.
- Workspace Settings `max_agents` default 16, clamped to the instance cap.
- `ensure_project` / first fleet GET uses 16, not 3.
- Remove or stop reading orch `MAX_AGENTS` (default 3).
- `FLEET_MAX_AGENTS` either goes away or becomes a bootstrap for the
  instance cap — pick one in the MR description and test it.
- `MAX_PROJECT_AGENTS` (256) becomes the instance cap (128 default,
  raiseable toward 1024 later) or is replaced by the instance row.

**Accept:** new workspace shows 16. Setting 200 with instance cap 128 is
rejected. Admin can raise the instance cap.

**Touches:** `fleet-db` migration, admin routes + UI, Settings,
`fleet_routes` / `chat_tools` `ensure_project`, orch `set_max_agents`.

---

### MR 4 — One migrator: `fleet-db` only (#23)

**Why:** two `sqlx::migrate!` share `_sqlx_migrations` with
`ignore_missing`. A version collision fails API startup (VPS included).

**Do**

- Copy `crates/fleet-orchestrator/migrations/*` into
  `crates/fleet-db/migrations/` **keeping the same version prefixes**
  (`20260728120000_…` etc.) so already-applied VPS/dev DBs no-op.
- Delete `sqlx::migrate!` from `fleet-orchestrator/src/store/pool.rs`.
- `fleet-server` startup runs only `fleet_db::migrate`.
- Integration tests that called orch `migrate` use `fleet_db` instead.
- Rewrite orch README (already points here).

**Accept:** empty DB + already-migrated DB both boot. No second
`_sqlx_migrations` writer.

**Risk:** high. Ship image + a scratch Postgres and a restored dump (or
VPS snapshot) before merge. Do not combine with MR 3’s new migration
until this has been on `main` once, **or** land MR 3’s SQL only in
`fleet-db` after this merge.

**Touches:** both `migrations/` trees, both `pool.rs`, server startup,
orch tests.

---

### MR 5 — Provider registry; easy to add DeepSeek (#16)

**Why:** five providers are hard-wired; decision is “all majors, easy to
add, DeepSeek when demand arrives.”

**Do**

- One registry (id, display name, where it runs: runner CLI vs
  control-plane key, status + complete hooks).
- Chat status / complete / Settings keys / guest `FLEET_HARNESS` map
  through the registry.
- Adding DeepSeek = one module + registry line + tests, not a sixth
  match-arm jungle.
- Implement DeepSeek in this MR **only** if the registry is smaller with
  a third control-plane-key example; otherwise stop at the registry and
  a failing-or-skipped “add provider X” checklist in the crate README.

**Accept:** a new provider does not require editing `chat.rs` complete
switch, `providers.rs` parse list, and harness match independently.

**Touches:** `fleet-server` `providers.rs` / `chat.rs` / `runner_chat.rs`,
`fleet-driver` harness, Settings llm-keys provider enum.

---

### MR 6 — Remove the sandbox lab leftover (#28)

**Why:** production guest is `fleet-host` → `fleet-driver`. Lab
(OpenCode/Pi/Redis compose) is leftover. Install script currently lives
under `sandbox-lab/`.

**Do, in order**

1. Move
   `crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`
   (and anything it sources) to `scripts/` (or keep a one-line wrapper
   that execs the new path). Update `AGENTS.md`, README, testing docs,
   install-contract tests.
2. Stop presenting lab invoices/OpenCode as “sandbox today”
   (`sandbox-workspace.md` already flagged historical).
3. Delete or quarantine lab compose jobs, Redis bus, harness images that
   only the lab uses — **after** `just test` / `run-full-suite.sh` have a
   path that does not require them, or those stages are explicitly
   retired.
4. Only then delete `crates/fleet-sandbox-driver` if nothing in install
   or Kind still links it.

**Do not** delete the install script in the same commit as the crate
without the move.

**Accept:** `install-full-suite-deps.sh` still exists at a documented
path. No README calls the lab the product sandbox. Kind/VPS still start
`fleet-driver`.

**Touches:** `scripts/`, `AGENTS.md`, `docs/testing.md`, sandbox-lab,
possibly `.gitlab-ci` (none today).

---

### MR 7 — Zenoh mid-run fabric (#12, #27, #29)

**Why:** decision is not “optional observability.” Sandboxes must share
mid-run (what others are doing, asks to the outside world) without a
single HTTP hop becoming the bottleneck at 100+ sessions. Event
catalogue is that bus’s payload. HTTP tick stays assignment + durable
board.

**Do, staged inside the MR or as 7a/7b if it gets huge**

1. **Version pin:** `eclipse/zenoh` image and workspace `zenoh` crate
   same major. Today: crate `1.10`, image `1.2.1`. Prove `open` + a
   pub/sub round-trip in CI/unit, not only “bus connected.”
2. **Contract:** map `docs/architecture/events.md` / 
   `agent-fleet-contracts` to what guests actually publish (not
   `SessionState` vs `spawning`/`healthy` confusion — either map or
   retire unused catalogue states).
3. **Guest path:** `fleet-driver` / `fleet-host` publish live progress
   and team-visible facts on Zenoh; subscribers are other permitted
   sessions in the same run/workspace.
4. **Plane:** HTTP `/v1` heartbeat/status remain durable proof.
   Tick does **not** wait on Zenoh to advance the board.
5. **AuthZ:** “who has permission to see this run” on the key space
   (workspace/run prefix). No public `zenohd` on the VPS NIC.
6. Tests: two fake guests, one post, the other sees it without going
   through `team_chat_post` HTTP; plane still ticks if `zenohd` is down
   (degrade, don’t deadlock assignment).

**Accept:**  N guests can observe each other via Zenoh; kill `zenohd`
and new claims still lease. Docs match.

**Touches:** `fleet-bus`, `fleet-host/src/bus.rs`, `fleet-server/src/bus.rs`,
`fleet-driver`, compose/studio/infra `zenohd` image, events.md,
contracts `SessionState`.

**Risk:** highest. Do not restage `FLEET_WORKER_DIST_DIR` until the new
`fleet-host` is known to start (AGENTS.md). VPS in-cluster host has
`auto_update=true`.

---

## Optional / later (not blocking the series)

| Item | Note |
| --- | --- |
| Cloud runners “first version” (#31) | Docs no longer say placeholder. A real SKU (billing, provision) is a product MR, not a contradiction fix. |
| Chat pause (#8) | Explicitly not a product. Stay deleted. |
| Public forge (#26) | Still deferred. Do not add `repository` to `Cargo.toml`. |
| Grok model pin (#17) | Stay unpinned. |

---

## Suggested sequence vs risk

```
this MR (landed)
    → O1 VPS CORS (after ship)
    → MR 1 isolation names
    → MR 2 persona names          (parallel with 1)
    → MR 4 single migrator
    → MR 3 instance/workspace caps  (after 4, or only-fleet-db SQL)
    → MR 5 provider registry        (parallel with 3)
    → MR 6 remove lab               (after install script move)
    → MR 7 Zenoh fabric             (last; needs stable host packages)
```

MR 1 and MR 2 do not share files much — can be two branches from `main`
after this MR merges.

---

## Per-MR checklist (copy)

Opening the GitLab MR **is** the green light to merge (`AGENTS.md`).
Do not create the MR until this list is done on the final tree.

- [ ] Decision numbers in the MR description
- [ ] `python3 tests/browser/surface_scope.py --write` (`--check` clean)
- [ ] Relevant `just test` / `just test-integration`
- [ ] Ship images: `Dockerfile` + `Dockerfile.ui` with `FLEET_GIT_SHA`
- [ ] Film `HUMAN_TEST_SCOPE.md` on the final tree; fill Film review
- [ ] No `git add` of videos, tokens, `.env`
- [ ] Do not merge unless a human asks
