# Inconsistency decisions

Fill in **Choice** under each item. Leave the other options so the discarded
path stays visible. If a later item is implied by an earlier one, write
`same as #3` on the Choice line.

**Impact** is 1–5: how bad it is if you pick wrong or leave it split.

| Score | Means |
| --- | --- |
| 5 | Prod lockout, paired-runner brick, or data/schema fork |
| 4 | Live users or VPS ship hit it (404, cookies, pair URL, health) |
| 3 | Everyday local/dev path is wrong; agents and humans follow it |
| 2 | Two live contracts disagree; implementers/tests fork |
| 1 | Naming or stale docs only; runtime already has one winner |

**Affected** is what you will touch or break. **VPS** is what
`us-vmmedium` / https://agentfleet.studio was actually doing on 2026-09-14.

**Inclination** (`N%:`) is how much I would pick that option, given the
code + VPS. Letter options on one decision add to 100%. Not a vote —
override it.

---

## What is running on the VPS (2026-09-14)

Inspected `agent-fleet@82.39.213.33` (`/home/agent-fleet/stack`) and the
public site. Secrets not copied here.

| | Live |
| --- | --- |
| SHA | `d0e0fc67` (`GET /health.git_sha` = UI `/fleet-git-sha.txt`). Previous image `bbb4c69` still on disk |
| Public | Cloudflare → Caddy `:80`/`:443`. API `127.0.0.1:8080`, UI `127.0.0.1:3030` |
| Containers | `postgres`, `api`, `ui`, `caddy`, `zenohd` (`eclipse/zenoh:1.2.1`), `host` (`fleet-host run`) |
| DB | Compose user/db **`fleet`**. Published only `127.0.0.1:5433→5432`. Not the `.env.example` URL |
| Stack env | **`/home/agent-fleet/stack/.env` only** (859 bytes). Deploy never overwrites it |
| Public bases | `API_PUBLIC_BASE=https://agentfleet.studio` · `UI_PUBLIC_BASE=https://agentfleet.studio` |
| CORS | `https://agentfleet.studio,https://www.agentfleet.studio` — both origins echo back on `/health` |
| www vs apex | **Both return 200, no redirect.** Caddy has no www→apex rule. Cookies have no `Domain` |
| `WORKER_PUBLIC_BASE` | **Unset.** Pair stored `control_plane_url = "https://agentfleet.studio"` |
| In-cluster host | Env `FLEET_CONTROL_PLANE_URL=http://api:8080` (compose DNS). `run_host=true`, `run_container=false`. Heartbeat `ok`. `auto_update=true`. `machine.id` since 2026-09-02 |
| Zenoh | `ZENOH_LOCATOR=tcp/zenohd:7447` on api + host. Logs: `control-plane bus connected` / `fleet-host bus connected`. Router 1.2.1, Rust crate 1.10 — session opens |
| Worker dist | On disk: `linux-x86_64` (+ bookworm), `linux-aarch64`, `windows-x86_64`. Bins: **`fleet-host` + `fleet-driver` only**. Public GET: linux 200; `windows-x86_64/fleet-host` 404 (likely wants `.exe`); musl/darwin 404 |
| Inbox | `GET …/fleet/tickets` → **404**. `GET …/inbox` → 401 (route exists) |
| `/api` index | Inbox is `/inbox`. Chat note: “Grok / Ollama / OpenAI / Claude”. `/v1` listed as `ADMIN_TOKEN` automation |
| Leftovers in `.env` | `SANDBOX_BACKEND=stub` (not passed into the api container). SMTP/Resend keys **empty** |
| Signup | `SIGNUP_TOKEN` is set (len 32) |

Kind `:8080`/`:3030` steal and `.env.local` are **local-dev only**. They do not apply on this VPS.

## Resolved (2026-09-14)

| # | Choice | Meaning |
| --- | --- | --- |
| 1 | B | `.env.example` uses `fleet@5433/fleet` |
| 2 | A | Canonical env file is `.env` only |
| 3 | B | Kind up → host API/UI `18080`/`13030`; commit that in docs + just |
| 4 | A | Loopback public/CORS URLs follow `PORT` / `FLEET_UI_PORT`; public hostnames stay |
| 5 | A | Two public bases: `API_PUBLIC_BASE` + `WORKER_PUBLIC_BASE` (unset → API) |
| 6 | A | Apex only; `www` redirects; drop `www` from CORS |
| 7 | A | Inbox path is `/inbox` |
| 8 | A | No chat pause; delete from README |
| 9 | A | Orchestrator is a library on `HOST`/`PORT` |
| 10 | A | Cookie BFF is the product API; `/v1` is worker + automation |
| 11 | A | OpenAPI `/v1` bodies follow the driver |
| 12 | Other | Zenoh is meant to carry lots of mid-run data (see #27) |
| 13 | B + Yes | Two axes, names follow function; Kind/container is assignable |
| 14 | C | Workspace persona vs roster specialist |
| 15 | A | Keep 8 starters |
| 16 | Other | All major providers; easy to add (DeepSeek when demand arrives) |
| 17 | A | Never pin a Grok model id |
| 18 | B | `team_chat_post` / `team_chat_list` + `kind` |
| 19 | A | Replace is HTTP-only; no `swap_agent` chat tool |
| 20 | A | Guest = composer + `mock`; Settings is no-chat fallback |
| 21 | C | `llm-keys` win when present; env remains fallback |
| 22 | Other | Instance hard cap default 128; per-workspace default 16 |
| 23 | A | Only `fleet-db` owns migrations (follow-up: move orch SQL) |
| 24 | B | README crate table is user-facing; full list in `Cargo.toml` |
| 25 | A | Delete the name `fleet-worker` |
| 26 | C | No public repo URL; GitLab is private; defer any public forge |
| 27 | Other | Zenoh is the mid-run share fabric for 100+ parallel sandboxes |
| 28 | A + remove lab | `fleet-driver` is the product guest; lab is leftover to remove |
| 29 | Other | Event catalogue is the Zenoh payload (#27), not a second HTTP plane |
| 30 | A | Product word is Runners |
| 31 | A | Cloud runners are a shipped first version; not a placeholder |
| 32 | A | One live UI map; rewrite stale “not built” docs |
| 33 | A | Invites/members are shipped |
| 34 | A | Bitbucket / Confluence are available |
| 35 | A | One narrative-docs pass after the above |

Date filled: 2026-09-14

Implementation plan for work **not** in the first consistency MR:
[`docs/inconsistency-follow-up-plan.md`](docs/inconsistency-follow-up-plan.md).

---

## Local setup

### 1. Postgres URL for local Compose

Copying `.env.example` and starting Compose Postgres does not connect.

| | User | Password | Host port | Database |
| --- | --- | --- | --- | --- |
| A `.env.example` | `postgres` | `postgres` | `5432` | `agent_fleet` |
| B Compose + `development.md` + VPS | `fleet` | `fleet` | `5433` | `fleet` |

**Impact: 3** — first-run / `just` / agent host `cargo run` fails. Prod already uses B.

**Affected:** `.env.example`, `development.md`, `docker-compose.yml` (if you pick A), `justfile` / `fleet-dev.sh` rewrite, orch test comments that still say `:5432`. Not the VPS `.env` (already `fleet`/`fleet`).

**VPS:** production is B (`postgres://fleet:…@postgres:5432/fleet`, host publish `127.0.0.1:5433`).

- [ ] A — change Compose (and docs) to `postgres@5432/agent_fleet` — **10%:** would churn VPS and every working compose URL for a leftover example
- [x] B — change `.env.example` (and leftover `5432/agent_fleet` comments) to `fleet@5433/fleet` — **90%:** Compose, development.md, and the VPS already agree
- [ ] Other: ________________________________

**Choice:**

### 2. Which env file is canonical for host runs

`AGENTS.md`, install, `fleet-dev.sh`, and `fleet-server` use **`.env`**.
`just` loads **`.env.local`** and `just setup` copies **`.env.local.example`** (file missing).

**Impact: 3** — `just setup` / `just test-integration` / `just dev` are broken or read a different file than `cargo run`.

**Affected:** `justfile`, `docs/testing.md`, `scripts/fleet-dev.sh`, `scripts/harvest-runner-catalog.sh`, `AGENTS.md`, install script, maybe a new `.env.local.example`. Not VPS (stack `.env` only).

**VPS:** one file, `/home/agent-fleet/stack/.env`.

- [x] A — `.env` only. Point `just` at `.env`. Delete `.env.local` references — **70%:** AGENTS, fleet-dev, server, and the VPS already use one `.env`
- [ ] B — `.env.local` only. Add the missing example. Point install / `fleet-dev` / server at it — **5%:** the example file does not exist and everything else would have to move
- [ ] C — both: `.env` for PROFILE/ENABLE_*/secrets, `.env.local` for just/dev overrides. Document and add the example — **25%:** useful if you want local overrides, but it is how this split started
- [ ] Other: ________________________________

**Choice:**

### 3. Host ports when Kind already owns `:8080` / `:3030`

Kind/k3d bind host `8080`/`3030`. `fleet-dev.sh` remaps to `18080`/`13030`.
Committed `Dioxus.toml`, `.env.example`, `just ui` still assume 8080/3030.

**Impact: 3** — live/e2e identity fails or you talk to Kind’s old images while thinking you hit the host tree. Fail-closed tests exist because of this.

**Affected:** `infra/kind.yaml`, `infra/k3d.yaml`, `Dioxus.toml`, `.env.example`, `justfile`, `scripts/fleet-dev.sh`, `docs/testing.md`, `AGENTS.md`, Playwright `FLEET_API_URL` / `FLEET_UI_URL`.

**VPS:** not this problem. Public is 80/443; api/ui are localhost-only 8080/3030 behind Caddy.

- [ ] A — documented ports stay 8080/3030. Kind must not steal them — **25%:** nicer docs, but Kind extraPortMappings are the published-cluster contract
- [x] B — when Kind is up, host stack is always 18080/13030. Commit that everywhere, not only in `fleet-dev.sh` — **65%:** fail-closed tests and fleet-dev already do this; only the committed files lie
- [ ] C — one pair forever (write them): host **and** Kind use `________` / `________` — **10%:** biggest infra churn for little gain
- [ ] Other: ________________________________

**Choice:**

### 4. Do `FLEET_UI_PORT` / `PORT` rewrite CORS and public bases

`development.md` says they follow the port knobs. Code only derives when
`API_PUBLIC_BASE` / `CORS_ORIGINS` are **unset**. `.env.example` sets them.

**Impact: 2** — changing the listen port locally does not move OAuth/CORS. Easy to debug for a day.

**Affected:** `crates/fleet-server/src/config.rs`, `.env.example`, `development.md`, `scripts/fleet-dev.sh` `derive_local_urls`.

**VPS:** knobs unset; public bases are explicit `https://agentfleet.studio`. Correct for prod.

- [x] A — derive always from `PORT` / `FLEET_UI_PORT` unless a public hostname is set — **65%:** local remaps work and a public hostname (VPS) stays explicit
- [ ] B — committed public/CORS vars are source of truth. Stop saying ports rewrite them — **35%:** matches today’s code, but then `FLEET_UI_PORT` is a trap
- [ ] Other: ________________________________

**Choice:**

---

## Public URLs (pair vs OAuth)

### 5. Worker phone-home URL vs OAuth public URL

| Variable | Who uses it |
| --- | --- |
| `API_PUBLIC_BASE` | OAuth + cookies |
| `WORKER_PUBLIC_BASE` | Pair `control_plane_url` (unset → `API_PUBLIC_BASE`) |
| `FLEET_URL` | `install-worker.sh` |
| `FLEET_CONTROL_PLANE_URL` | Host process after pair / compose override |
| `ORCHESTRATOR_PUBLIC_URL` | Orch public-URL derivation |

Docs mix these. `development.md` promises unset worker base = LAN IP. Code does not invent a LAN IP.

**Impact: 5** — wrong URL lockouts paired laptops (AGENTS.md: do not force re-pair). Auto-update downloads from `control_plane_url`.

**Affected:** `worker_hosts.rs` pair payload, install scripts, `fleet-host` `config.toml`, guest `FLEET_*` URLs, `.env.example`, worker-host README, PLAY.md, `fleet-dev.sh`, VPS `.env` only if you add `WORKER_PUBLIC_BASE`.

**VPS:** `WORKER_PUBLIC_BASE` unset → pair stored `https://agentfleet.studio`. In-cluster host **runtime** uses `FLEET_CONTROL_PLANE_URL=http://api:8080` (compose DNS), which is why the VPS host can heartbeat without looping through Cloudflare. External boxes must use the public HTTPS URL. That split is already how prod works.

- [x] A — keep two public bases. Document only `API_PUBLIC_BASE` + `WORKER_PUBLIC_BASE`. Unset worker base stays `API_PUBLIC_BASE` (current code + current VPS) — **75%:** VPS pair is already HTTPS public + compose host overrides with `FLEET_CONTROL_PLANE_URL`
- [ ] B — unset `WORKER_PUBLIC_BASE` must become LAN IP + `PORT` (implement the doc). Do **not** do this on the VPS — **5%:** a LAN default would be wrong the moment you ship
- [ ] C — one public base. Delete `WORKER_PUBLIC_BASE` / `ORCHESTRATOR_PUBLIC_URL` — **5%:** the compose host must phone `http://api:8080`, not the public HTTPS name
- [ ] D — keep all five names; one table in `.env.example`; delete alias docs — **15%:** honest, but five names is the bug
- [ ] Other: ________________________________

**Choice:**

### 6. Studio apex vs `www`

**Impact: 4** — login on one host is logged out on the other. OAuth callback is apex (`API_PUBLIC_BASE`). CORS allows both, so the browser can call the API from `www` but cookies stay host-only.

**Affected:** `deploy/studio/env.example`, **VPS `/home/agent-fleet/stack/.env` `CORS_ORIGINS`**, `deploy/studio/Caddyfile` (no redirect today), Cloudflare DNS (both A records exist), `fleet-auth` cookies (`Domain` unset), OAuth app callback URLs.

**VPS:** apex and `www` both 200, no redirect. CORS reflects both origins. Caddy `redir` is only HTTP→HTTPS, not www→apex.

- [x] A — apex only. Drop `www` from CORS; Caddy (or CF) redirects `www` → apex — **80%:** OAuth and public bases are already apex; one redirect fixes the split session
- [ ] B — `www` only. Flip public bases; apex redirects — **5%:** retargets every OAuth callback for no product reason
- [ ] C — both first-class. Set cookie `Domain=.agentfleet.studio` and decide a canonical OAuth host — **15%:** more cookie/OAuth surface than this site needs
- [ ] Other: ________________________________

**Choice:**

---

## HTTP surfaces

### 7. Inbox API path

README: `GET /api/workspaces/{id}/fleet/tickets`.
Code + UI + live `/api` index: `/inbox`.

**Impact: 4** — documented path 404s in production (`GET …/fleet/tickets` → 404; `…/inbox` exists).

**Affected:** root `README.md` only for the doc fix (A). UI `pages/inbox/api.rs` already `/inbox`. Tests `inbox_state_query.rs`.

**VPS:** confirmed 404 vs 401.

- [x] A — `/inbox` is the path. Fix README — **95%:** UI, tests, `/api` index, and the VPS 401/404 already say so
- [ ] B — restore `/fleet/tickets` (alias or move) — **5%:** rename for a stale README line
- [ ] Other: ________________________________

**Choice:**

### 8. Chat pause

README documents `POST …/chat/conversations/{cid}/pause`. No route.

**Impact: 2** — anyone implementing pause from the README wastes a cycle. No UI button today.

**Affected:** `README.md`. Implement path would be `fleet-server` chat router + UI composer + tests.

- [x] A — not a product. Delete from README — **85%:** no route, no button; this is leftover copy
- [ ] B — implement pause — **15%:** real feature, but it is not required to resolve the contradiction
- [ ] Other: ________________________________

**Choice:**

### 9. Orchestrator as a process

`.env.example` / orch README first half: `cargo run -p fleet-orchestrator` and `HTTP_ADDR=:8081`.
Crate is a library. `fleet-server` binds `HOST`/`PORT`. `demo-orch.sh` defaults to `:8081`.

**Impact: 3** — advertised command does not start. Demo script misses the server. Not a VPS outage (`/v1` is already on `:8080` behind Caddy).

**Affected:** `crates/fleet-orchestrator/README.md`, `.env.example`, `scripts/demo-orch.sh`, OpenAPI `servers:`, `just orch`. No VPS compose change if you pick A.

**VPS:** only `fleet-server :8080`. Caddy proxies `/v1*` to api. No `:8081`.

- [x] A — library only on `HOST`/`PORT`. Delete standalone-run / `:8081` / `HTTP_ADDR` as listen. Point demo at `:8080` — **90%:** no binary, VPS has no `:8081`, Caddy already proxies `/v1` to api
- [ ] B — bring back a standalone binary on `:8081` — **10%:** a second process to operate for no isolation win
- [ ] Other: ________________________________

**Choice:**

### 10. Which orchestrator HTTP API is the product API

Cookie BFF `/api/workspaces/{id}/fleet/*` vs bearer `/v1/projects/{id}/*`. Different fixture bodies.

**Impact: 3** — OpenAPI clients and the UI cannot share a fixture. Live index already labels `/v1` as “automation / workers”.

**Affected:** `openapi.yaml`, `fleet_routes.rs`, UI `fleet/api.rs`, `demo-orch.sh`, orch README, `/api` index copy.

**VPS:** both mounted on the same process. UI uses cookies. `/v1` is for guests (`fleet-driver` heartbeats) and optional `ADMIN_TOKEN`.

- [x] A — cookie BFF is the product API. OpenAPI documents that (or dies). `/v1` stays worker + automation — **70%:** the UI and humans already live here; `/v1` is the guest heartbeat
- [ ] B — `/v1` is the real API; UI should match those bodies — **5%:** rewrite the app for an admin-token surface
- [ ] C — keep both; two specs; stop implying they are the same — **25%:** honest if you will not rewrite OpenAPI this month
- [ ] Other: ________________________________

**Choice:**

### 11. OpenAPI worker `/io` + heartbeat/status bodies

Spec `op` enum is incomplete. Heartbeat/status omit `events` / `result_md` / `files` that `fleet-driver` sends.

**Impact: 2** — generated clients drop fields the plane uses for traces/`result.md`. Live driver is fine.

**Affected:** `crates/fleet-orchestrator/api/openapi.yaml`, maybe `worker_io.rs` / `handlers.rs` if you drop aliases, `fleet-driver` `orch.rs`.

- [x] A — spec follows the driver — **40%:** cheapest fix if you do not want to rename tools yet
- [ ] B — driver follows the spec — **10%:** would strip `events` / `result_md` the plane uses
- [ ] C — one name per op (see #18) and regenerate OpenAPI — **50%:** do this with the team-chat naming decision so you only rewrite once
- [ ] Other: ________________________________

**Choice:**

---

## Shared types / wire contracts

### 12. Session lifecycle enum

Contracts/events.md: `pending` … `archived`.
Live API/UI: `spawning` / `healthy` / `stuck` / `killing` / `exited` / `failed_spawn`.

**Impact: 2** — a contracts consumer cannot read `/fleet/sessions`. Host Zenoh still publishes the unused set (harmless if nothing subscribes).

**Affected:** `agent-fleet-contracts` `SessionState`, `docs/architecture/events.md`, `fleet-host/src/bus.rs`, orch `WorkerSessionStatus`, UI sandboxes/fleet badges.

**VPS:** HTTP sessions use the live set. Zenoh bus is connected but tick does not consume catalogue states.

- [ ] A — live set wins; change contracts + events.md + host bus — **55%:** `/fleet/sessions` and the UI already speak `spawning`/`healthy`
- [ ] B — catalogue wins; change orch DB/API + UI — **5%:** a status-machine rewrite for unused catalogue names
- [ ] C — two concepts, two names, one explicit map — **40%:** keep the catalogue only if you still want a future event plane (#29)
- [x] Other: the idea was to use zenoh to communicatecommunicate a lot of data. it was supposed to be a central piece of this product

**Choice:**

### 13. Isolation vocabulary

Workspace `min_isolation`: `process`/`container`/`vm`.
Host `isolation`: `native`/`sandboxed`.
Host `run_mode`: `on_pc`/`in_container`.

**Impact: 4** — Settings PATCH with the other enum 409s. Wrong floor can refuse every host or schedule onto the wrong class. Kind in-cluster host is container.

**Affected:** `fleet-db` workspace + worker_host models, Settings UI, host README, migration comments (`015` “not assignable”), Kind `infra/base/host.yaml` (`RUN_HOST=false`, `RUN_CONTAINER=true`), picker tests.

**VPS in-cluster host:** `run_host=true`, `run_container=false`, `container_backend=k3d` leftover in `config.toml`. It is a **process** guest on the VPS box, not KubePod.

- [ ] A — one enum: `process`/`container`/`vm`. Map host native→process, sandboxed→container — **25%:** simpler, but collapses “how isolated” and “on this PC vs in a container”
- [x] B — two axes, two names (floor vs where it runs). Stop sharing the word “isolation” — **70%:** Settings already treats them as independent; the 409s come from the shared word
- [ ] C — host `native`/`sandboxed` is the product language — **5%:** users see process/container/vm in Settings
- [ ] Other: ________________________________

it must be two axes because the agent can run on the host with TUI or on a container/VM with API tokens, and the repo can live on the host or on a container/VM - naming must be adjusted for functinality, not the other way around


Also: is Kind/container assignable in v1?

- [x] Yes — delete “not assignable / no k8s in v1” — **85%:** Kind host + assignable-host tests already do this
- [ ] No — stop assigning sandboxed+container (including Kind host) — **15%:** would disable the in-cluster path you ship

**Choice:**

### 14. Personas: workspace catalog vs agency-agents roster

Two JSON shapes, one word.

**Impact: 3** — Agents page vs `list_agent_personas` vs `/personas` CRUD do not compose. Chat cannot see `role_prompt` from the roster tool.

**Affected:** `003_personas.sql`, `fleet-auth` personas API, `fleet-orchestrator/src/personas.rs`, Agents UI DTOs, `chat_tools.rs` `list_agent_personas`, docs/personas.

- [ ] A — one type = workspace schema — **20%:** loses division/emoji roster cards the Agents page already shows
- [ ] B — one type = roster schema — **5%:** drops role_prompt/tools/budget you persist in Postgres
- [x] C — two types, two names (workspace persona vs roster specialist) — **75%:** they are already two JSON shapes; the bug is the shared noun
- [ ] Other: ________________________________

**Choice:**

### 15. Seed starters count / source of truth

**Impact: 1** — docs already say 8 and tests insert 8. Only a product change if you want roster-only.

**Affected:** `seed_starters` in `fleet-auth`, Agents UI copy, README.

- [x] A — keep 8 starters — **90%:** API, tests, and UI copy already match
- [ ] B — starters = agency-agents roster only — **10%:** a product change, not a contradiction fix
- [ ] C — other: ________________________________

**Choice:**

---

## Tools, models, harnesses

### 16. Chat providers that are first-class

Code accepts five. Status without `runner_id` returns only OpenAI + Gemini.
Live `/api` chat note: “Grok / Ollama / OpenAI / Claude” (no Gemini).

**Impact: 3** — composer empty/wrong until a runner is selected; docs disagree with the index and the handler.

**Affected:** `chat.rs` `chat_status`, `providers.rs`, README, `fleet-server/README.md`, `fleet-ui` chat composer, live `/api` index string.

**VPS:** same binary family; no extra chat env (no `GROK_ENABLED` etc. in stack `.env`).

- [ ] A — five providers. Fix READMEs. Status without a runner lists all five (or explains cloud-only) — **25%:** handler accepts five, but status-without-runner is not that list
- [ ] B — runner providers vs control-plane-key OpenAI/Gemini, documented as such — **70%:** matches `chat_status` today; stop promising Grok in the no-runner payload
- [ ] C — shrink to Grok + Ollama — **5%:** would delete Claude/OpenAI/Gemini paths that already run
- [x] Other: we must support ALL major providers, including ones not listed like deepseek. it must be easy to add more providers in the future, and demand arives

**Choice:**

### 17. Default Grok model

**Impact: 2** — hard-coded `grok-4.5` in docs/tests vs CLI default `grok-4.6`. Runtime does not pin.

**Affected:** README, `grok.rs` `default_model()`, host `agents.rs` fixtures, orch tests, `behavior-interactions.md`.

- [x] A — never pin; docs say first `grok models` id — **80%:** `default_model()` and the grok.rs comment already refuse to pin
- [ ] B — pin `grok-4.6` (or ________) — **15%:** today’s CLI default, stale the day the CLI ships another
- [ ] C — pin `grok-4.5` in `default_model()` — **5%:** pins the name the README already got wrong
- [ ] Other: ________________________________

**Choice:**

### 18. Worker / human team-chat tool names

Docs `send_message`/`ask_teammate` vs chat tools vs `/io` vs `fleet-tool.sh`.
No `op=ask_teammate`.

**Impact: 3** — a worker that follows MULTI_AGENT.md gets 400. Mid-run Q&A does not work as documented.

**Affected:** `worker_io.rs`, `chat_tools.rs`, `team_chat.rs`, `guest_files.rs` `fleet-tool.sh`, MULTI_AGENT.md, OpenAPI, ollama prompt.

- [ ] A — product names `send_message` / `ask_teammate` / `read_messages`; implement ask — **20%:** what the docs promised, but `/io` has no ask op yet
- [x] B — product names `team_chat_post` / `team_chat_list` + `kind` — **45%:** what the driver, `fleet-tool.sh`, and `/io` already speak
- [ ] C — drop ask-and-wait; FYI post + list only — **35%:** ask is fiction today; deleting it is cheaper than implementing it
- [ ] Other: ________________________________

**Choice:**

### 19. `swap_agent` / Replace specialist

Chat tool missing; HTTP `…/sessions/{id}/replace` **is** mounted (`workforce::router`).

**Impact: 2** — button can work; chat/`swap_agent` docs lie. Not a 404.

**Affected:** `runner_chat.rs`, MULTI_AGENT.md, `chat_tools.rs`, Fleet Replace button (already wired).

- [x] A — HTTP-only Replace. Delete `swap_agent` from chat/docs — **70%:** the button already hits a live route; the chat tool is the leftover
- [ ] B — implement `swap_agent` → same replace path — **25%:** only if you want the model to replace specialists mid-chat
- [ ] C — remove Replace entirely — **5%:** would delete a working control
- [ ] Other: ________________________________

**Choice:**

### 20. `FLEET_HARNESS` vs Settings `fleet_llm` vs `FLEET_AGENT_BACKEND`

Three knobs, three allowed sets.

**Impact: 3** — fallback harness when chat has no model; VPS host is `FLEET_HARNESS=mock`.

**Affected:** `fleet-driver` README + `harness.rs`, Settings PATCH, `agent_loop.rs`, VPS/compose `FLEET_HARNESS`, `.env.example`.

**VPS:** `FLEET_HARNESS=mock` on the in-cluster host.

- [x] A — guest = composer provider + `mock`; Settings is no-chat fallback — **65%:** matches the product README (chat stamps the run; Settings is fallback)
- [ ] B — guests only `mock`/`grok`/`claude`/`ollama` — **25%:** drops openai/gemini harness aliases that barely belong on a box
- [ ] C — Settings can select every guest harness — **10%:** fights “Grok is env-only” and the ollama-only `fleet_llm` column
- [ ] Other: ________________________________

**Choice:**

### 21. Workspace LLM keys vs “Grok is env-only”

**Impact: 2** — Settings copy vs `POST /llm-keys`. Secrets storage policy.

**Affected:** Settings UI copy, `fleet_routes` `fleet_llm` validation, `llm_keys` routes, `035_workspace_llm_keys.sql`.

- [ ] A — Grok env/CLI only; reject `grok` on `llm-keys` — **15%:** would shrink an API you already shipped
- [ ] B — `llm-keys` is how cloud secrets are stored; stop saying env-only — **35%:** the table exists; the copy is the lie
- [x] C — both; keys win when present — **50%:** spawn already checks `workspace_llm_keys` and env remains the VPS fallback
- [ ] Other: ________________________________

**Choice:**

---

## Capacity

### 22. Concurrent-agent default and cap

`ensure_project` hardcodes **3**. `FLEET_MAX_AGENTS` default 16, cap 256.
Docs once say cap 1000 (that is plan size).

**Impact: 3** — new workspace capacity is 3 until Settings, even if env says 16. Wrong 1000 in docs is harmless.

**Affected:** `chat_tools.rs` / `fleet_routes.rs` `ensure_project`, Settings UI, `.env.example`, `MAX_AGENTS` in orch config, `behavior-interactions.md`.

**VPS:** neither `FLEET_MAX_AGENTS` nor `MAX_AGENTS` in stack `.env` → code defaults.

- [ ] A — new project = `FLEET_MAX_AGENTS` (default 16, cap 256). Delete `MAX_AGENTS` — **35%:** what people think the env var means
- [ ] B — new projects stay 3; env is raise-floor for chat only. Document it — **55%:** that is `ensure_project` + GREATEST today
- [ ] C — one number, default 3, cap 256 everywhere — **10%:** fights the Settings default of 16
- [x] Other: there must be a instace wide hard cap in the instance settings, default 128, and a setting per workspace, default 16 - the count is per workspace (in the future this might grow to 1024+)

**Choice:**

---

## Persistence / crates

### 23. Who owns SQL migrations

**Impact: 4** — two `sqlx::migrate!` on one `_sqlx_migrations`. A future version collision or “move everything to fleet-db” done halfway can fail API startup (VPS included).

**Affected:** `fleet-db/migrations`, `fleet-orchestrator/migrations`, both `pool.rs`, `fleet-db/README.md`, every environment that migrates on boot (VPS api included).

**VPS:** api already ran both (ignore_missing). Changing ownership needs a careful ship.

- [x] A — only `fleet-db/migrations`. Move orch SQL; delete orch `migrate!` — **70%:** one `_sqlx_migrations` owner is the only safe long-term rule
- [ ] B — two migrators is the design; rewrite the README + version rules — **30%:** cheaper now, still a startup footgun on the VPS
- [ ] Other: ________________________________

**Choice:**

### 24. Workspace crate list in README

**Impact: 1** — onboarding map is incomplete. Runtime fine.

**Affected:** root `README.md` table.

- [ ] A — list every workspace member — **45%:** complete, but the table becomes an internal map
- [x] B — table = user-facing crates; one line pointing at `Cargo.toml` — **55%:** README is a product map; `Cargo.toml` is the member list
- [ ] Other: ________________________________

**Choice:**

### 25. `fleet-worker` leftover name

**Impact: 2** — install/docs say a bin that is not staged. Auto-update package list is `fleet-host` + `fleet-driver`.

**Affected:** `fleet-host` README + `Cargo.toml` description, `fleet-driver` README/`main.rs`, `PACKAGE_BIN_NAMES`, VPS `worker-dist/*`.

**VPS:** staged files are `fleet-host` + `fleet-driver` (and `.exe` on Windows). No `fleet-worker`.

- [x] A — delete the name `fleet-worker` — **90%:** VPS dist and `PACKAGE_BIN_NAMES` are already host+driver
- [ ] B — rename `fleet-driver` → `fleet-worker` (bins + dist + auto-update) — **10%:** live auto-update of every paired box
- [ ] Other: ________________________________

**Choice:**

### 26. Cargo `repository` URL

**Impact: 1** — crates.io/docs metadata. Development is GitLab.

**Affected:** workspace `Cargo.toml` `repository`.

- [ ] A — GitLab URL — **70%:** this is where MRs and `origin` actually are
- [ ] B — leave GitHub as public/canonical — **30%:** fine if GitHub is a mirror you still want on crates
- [x] C — other: the gitlab repo is private. it must not be public anywhere. the code is private so far. there is nothing public yet. if we are going to need a public space other than the website itself in the future, push this decision forward

**Choice:**

---

## Two products in one repo

### 27. Zenoh

Docs: not a locked decision. Prod **runs it**. Clients 1.10, router **1.2.1**. Tick does not consume the bus. Host + api logs say bus connected.

**Impact: 4** if you turn it off or “fix” the version without checking pair/update (bus is on the hot path of process start). **2** if you only clean docs. Version skew has not blocked the current VPS session open, but it is undefined.

**Affected:** `docker-compose.yml` `zenohd`, `deploy/studio/compose.yml`, `infra/base/zenohd.yaml`, `ZENOH_LOCATOR` on api/host, `fleet-bus`, `fleet-server/src/bus.rs`, `fleet-host/src/bus.rs`, `transport-zenoh.md`, architecture README. **VPS compose + 11-day-old `zenohd` container.**

**VPS:** `eclipse/zenoh:1.2.1` up 11 days; api `zenoh-1.10.0`; both “bus connected”. Not published on the public NIC.

- [ ] A — not the control plane. Stop shipping `zenohd` in Compose/Kind/studio. Docs become a future note or go away — **25%:** tick does not consume it; ripping it out of a live VPS still needs a careful ship
- [ ] B — required. Pin router + crate to the same major (which? ________). Decide if tick consumes it — **15%:** a real architecture bet you have not started
- [ ] C — optional observability only. Keep opt-in `ZENOH_LOCATOR`. Align versions. Architecture README must not read as if the plane is Zenoh — **60%:** VPS bus is connected; do not cold-kill it, do not pretend it drives sessions
- [x] Other: the idea is that the sandboxes will share information mid run, while they work together. and they will use zenoh for this. for example, one agent is working on something, but needs something from the world outside - it uses zenoh to communicate. that was the idea because we want to be able to run 100+ sessions in parallel, so if w have a central distributing point, that becomes a bottleneck. we need everybody (who has permission) to be able to see what the others are doing and how it is going in real time

**Choice:**

### 28. Two sandbox guests

**Impact: 3** — architecture “sandbox today” is the lab; prod guest is `fleet-driver` on `fleet-host`. Mixing them in a change set ships the wrong binary.

**Affected:** architecture README + `sandbox-workspace.md`, `fleet-sandbox-driver` crate, VPS/compose `host` command (`fleet-host` → `fleet-driver`), `FLEET_HARNESS`.

**VPS:** only `fleet-host` + `fleet-driver`. No Redis, no OpenCode lab.

- [x] A — `fleet-driver` is the product guest. Lab is an experiment; rewrite “sandbox today” — **70%:** VPS and pair path only ever start `fleet-driver`
- [ ] B — lab driver is the product guest; `fleet-driver` is the stand-in — **5%:** no Redis/OpenCode on the VPS; would invert production
- [ ] C — both ship under two names — **25%:** keep the lab for `run-full-suite`, but never call it “sandbox today”
- [ ] Other: ________________________________

We must remove the Lab thing - it is a leftover


**Choice:**

### 29. Event catalogue vs HTTP tick

**Impact: 2** — new work aimed at `events.md` will not move the board. Prod is HTTP tick + `/v1` reports + `/api/worker-hosts`.

**Affected:** `docs/architecture/*`, contracts message types, host bus publishes.

**VPS:** ticker `tick_interval_s=5`; heartbeats on HTTPS `/api/worker-hosts/me/heartbeat`.

- [ ] A — HTTP tick is the architecture; shrink/archive events.md — **35%:** matches what moves the board today
- [ ] B — catalogue is the target; HTTP is a temporary adapter — **10%:** no work is aimed at emitting those events
- [ ] C — HTTP/Postgres is durable; events are an optional projection. Say that in paragraph one — **55%:** keep the catalogue without letting new work treat it as the plane
- [x] Other: Isn't zenoh for this? see #27

**Choice:**

---

## Product IA and stale UI copy

### 30. Workers vs Runners

**Impact: 3** — first-run docs send people to a redirect. Nav word is Runners.

**Affected:** README, `development.md`, sidebar, `/{ws}/workers` redirect, human-test, worker-host README title.

- [x] A — product word is Runners. Fix docs. Keep redirect — **80%:** that is the shipped nav; `/workers` is already a redirect
- [ ] B — product word is Workers. Nav goes back — **5%:** undo a rename the UI already did
- [ ] C — both words, one-sentence split, apply it — **15%:** only if you want “worker = machine, runner = slot”
- [ ] Other: ________________________________

**Choice:**

### 31. Cloud runners page

**Impact: 2** — README says placeholder; attach API is live. Users may or may not be meant to click it.

**Affected:** README row, `runners/cloud` page, `POST …/runners/cloud/fleet`.

- [x] A — shipped; delete “placeholder” — **45%:** the attach route is live, so the README is false
- [ ] B — not shipped; hide/disable attach — **55%:** I have not seen a real Fleet Cloud SKU behind that button
- [ ] Other: ________________________________

It is time to ship a first version of this


**Choice:**

### 32. Stale “not built yet” UI docs

**Impact: 2** — `docs/components/ui/README.md` describes a different app. Agents following it skip real pages.

**Affected:** `docs/components/ui/README.md`, `crates/fleet-ui/README.md` status table.

- [x] A — rewrite to match live routes — **75%:** people still land on that README; make it true
- [ ] B — move under `docs/references/` as historical — **25%:** fine if `crates/fleet-ui/README.md` is the only map
- [ ] Other: ________________________________

Ideally we should have a single source of truth if possible


**Choice:**

### 33. Invites / members

**Impact: 2** — `behavior-interactions.md` says the API does not exist. Settings already has it.

**Affected:** `docs/behavior-interactions.md`, `fleet-auth` members/invites, Settings UI. Not a VPS schema question (migration `019` already applied wherever api migrated).

- [x] A — shipped; fix the behavior doc — **90%:** Settings + migrations + routes are already in
- [ ] B — not product; remove API/UI — **10%:** deleting a live members page to match a stale sentence
- [ ] Other: ________________________________

**Choice:**

### 34. Bitbucket / Confluence

**Impact: 2** — Connect may show in the catalog while the integrations README says coming soon.

**Affected:** `docs/components/integrations/README.md`, `catalog.rs` comment + availability, catalog unit test, `.env.example` (`BITBUCKET_*` / `CONFLUENCE_*` missing today).

- [x] A — available; fix README + comment — **70%:** catalog + unit test already require `Available` and `/oauth/start`
- [ ] B — coming soon; flip availability and the test — **30%:** pick this if those OAuth apps are not actually usable
- [ ] Other: ________________________________

**Choice:**

### 35. Docs sweep after the choices above

**Impact: 1** as a standalone decision — it is the cleanup mandate, not a product fork.

**Affected:** README, `development.md`, crate READMEs, `docs/components/*`, `behavior-interactions.md`, MULTI_AGENT.md — only the files implied by your other choices.

- [x] A — one pass updates all narrative docs after #1–#34 — **65%:** otherwise the next agent will rediscover the same lies
- [ ] B — only fix files that 404 or fail connect — **35%:** faster; leaves MULTI_AGENT / UI component docs stale
- [ ] Other: ________________________________

**Choice:**

---

## Suggested order (by impact)

Do these first; several later items collapse into them.

| # | Topic | Impact | I lean |
| --- | --- | --- | --- |
| 5 | Worker vs OAuth public URL | 5 | A 75% |
| 6 | apex vs www cookies | 4 | A 80% |
| 7 | Inbox path | 4 | A 95% |
| 13 | Isolation vocabulary | 4 | B 70% (Kind assignable: yes 85%) |
| 23 | Migration ownership | 4 | A 70% |
| 27 | Zenoh keep/kill/version | 4 | C 60% |
| 1 | Postgres URL | 3 | B 90% |
| 2 | Env file | 3 | A 70% |
| 3 | Kind vs host ports | 3 | B 65% |
| 9 | Orch as a process | 3 | A 90% |
| 10 | Which HTTP API | 3 | A 70% |
| 14 | Personas | 3 | C 75% |
| 16 | Chat providers | 3 | B 70% |
| 18 | Team-chat names | 3 | B 45% (C 35% close) |
| 20 | Harness knobs | 3 | A 65% |
| 22 | Capacity default | 3 | B 55% |
| 28 | Two sandbox guests | 3 | A 70% |
| 30 | Workers vs Runners | 3 | A 80% |

Date filled: ________
Filled by: ________
