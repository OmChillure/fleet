# Human testing and MR demos

Automated tests (`docs/testing.md`) do not replace a person looking at the
product, and they do not produce the **demo video** a merge request needs.

Two roles:

| Who | What they do |
| --- | --- |
| **Human** (author or reviewer) | Run `./scripts/human-test.sh`. It starts Postgres + API + UI and walks **HUMAN_TEST_SCOPE.md** only. Confirm those surfaces still make sense. |
| **Agent** (the coding agent on the MR) | Keep `HUMAN_TEST_SCOPE.md` in sync (`surface_scope.py --write`) whenever the diff changes. Record those scenes on the **final merge tree**. **Review the film** (frames + `.webm`) for bugs, fill the Film review section, attach the `.webm`. Never commit videos. |

Install is still one script with no flags
(`crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`)
and repo-root `.env`. This page does not install packages.

---

## When this applies

| Change | Human walkthrough | Agent demo video | GitLab MR + peer review |
| --- | --- | --- | --- |
| User-visible UI, API behavior, data, install, tests, sandbox/lab | Yes — **HUMAN_TEST_SCOPE.md** steps only | Yes — those scenes on the final merge tree, then **review the film** | **Required** |
| Operator-only (kind, compose job, workspace agent) | Yes, the relevant step | Yes — `lab` or `kind` scene on the final merge tree | **Required** |
| Small tweak: typo, comment nit, one-line docs wording, import order | No | No | Optional (direct to the target branch is OK) |

If it can break a user, a sandbox, or the next install, it is **not** a small
tweak. Prefer an MR and a Slack ask for review even when you could
push.

There is **no GitLab runner**. Keep `.gitlab-ci.yml` as `when: never`. Tests
and demos run on a developer or agent host.

---

## Humans: launch and walk the product

From the repo root (after install has been run once):

```bash
./scripts/human-test.sh
```

That command:

1. Refuses to invent a second install. Missing `cargo` / `docker` / `dx` →
   run the install script, then come back.
2. Starts compose Postgres, `fleet-server`, and `fleet-ui` via
   `./scripts/fleet-dev.sh start` if they are not already up. Default
   ports are `:8080`/`:3030`. If Kind already owns those and they are
   not this tree, the host stack binds `:18080`/`:13030` and writes
   `.run/host-urls.env` (`FLEET_API_URL` / `FLEET_UI_URL`).
3. Opens the UI when it can (`xdg-open` / `open`).
4. Pauses on each step below. Press Enter after you have checked the screen.

The list below is the **catalog**. `./scripts/human-test.sh` walks only
the pause steps in **HUMAN_TEST_SCOPE.md** (regenerate with
`python3 tests/browser/surface_scope.py --write` after every code
change — the map follows direct and indirect path hits). `guide-all`
is a first product tour only. Still do **This merge request**.

Subcommands: `launch`, `guide` (no start), `status`, `stop`, `record` (agents).

Use `http://localhost:3030` (or the `FLEET_UI_URL` host port, not
`127.0.0.1`) so cookies match CORS.

**Port-up is not this tree.** Kind often binds host `:8080`/`:3030` to old
`agent-fleet-*:local` images. `human-test.sh` / `record` / browser e2e
require `GET /health` → `git_sha` == `scripts/tree-id.sh`, UI
`/fleet-git-sha.txt` == that id, and `apis` contains `chat`. After code
changes, re-run the install script or rebuild before filming. Film only
the **final tree that will be merged** (identity matches that tree; no
edits after the clip). Existing `.webm`s filmed without that pair, or
shot before later edits, are **not** MR proof — discard them.
There is no green path via `FLEET_ALLOW_FOREIGN_STACK`. Missing host `dx`
is a fail (Kind UI is not a substitute).

Throwaway accounts only: `*@example.test`. Do not type real emails, tokens,
or customer data. Do not persist OAuth against a personal GitHub in a shared
DB unless that is the MR.

When you add a user-visible surface, **add a step** to
`scripts/human-test.sh` and a scene to `tests/browser/record_demo.py` in the
same change. The walkthrough is allowed to go stale only if the product did.

### Walkthrough (what the script asks)

1. **Landing** `/` — hero, Get started, Log in.
2. **Signup** — valid account; you land on login, not inside the app.
3. **Login** — dashboard of a workspace (`/{uuid}/dashboard`). Title is
   **Workforce**. Shows set-a-goal, open goals, open runs, live
   agents, team board, and team chat (empty until a run). An in-progress
   card has **Watch** → Live session.
4. **Unauthenticated deep link** — incognito or logged-out `/{uuid}/dashboard`
   redirects to `/login`.
5. **Sidebar** — Chat (New chat, Chat history), Monitoring (Workforce, Fleet,
   Sandboxes, Cases, Inbox), Agents (All agents, Live sessions), Runners (Self-hosted,
   Cloud, Partners), Workspace (Integrations, Settings). Expand collapsed
   sections. `/workers` still redirects.
6. **Workspace switcher** — list; create a second workspace; switch.
7. **Settings** — concurrent agents + Fleet LLM (Ollama). No git URL.
    Members: invite by email (member/admin). Existing users join immediately;
    others join on signup or next login. Owner can change roles / remove;
    member cannot invite.
8. **Runners / Self-hosted** — `/{ws}/runners/self-hosted`. Empty is **No
   workers yet** (the API is not a runner). Add a worker; paste the
   install command on the machine (this one is fine). Token panel is
   success — do not require a second machine. On a runner with Ollama:
   disk free, a searchable current top-50 popular-model list, and an
   Unlisted field for any name/URL. `/{ws}/workers` redirects here.
9. **Cloud** — `/{ws}/runners/cloud`. Placeholders for public cloud
   (AWS / GCP / Azure) and Fleet Cloud. Do not enter provider keys.
10. **Partners** — `/{ws}/runners/partners`. Trust copy + disclaimer;
    acknowledge does not enable lending. Lend / buy stay disabled.
11. **Agents** — Seed starters (8 personas). Creating/archiving is optional.
12. **Integrations** — catalog. Connect only if this MR is OAuth and secrets
    are in `.env`.
13. **Chat** — provider/model dropdown defaults to last used if that pair is
    ready on the selected runner, else the first installed/ready provider.
    Ollama is labeled **Private (Ollama)**. Claude uses official aliases
    (do not run `claude models`). After a reply the bubble shows send →
    response time. Send a line only if Grok or Private (Ollama) is actually
    up. Otherwise still open the page (empty composer is success).
    Plain language is enough (read Slack, implement, push GitHub, write Notion). Connect apps first.
    Inbox tickets: Start monitor, then Finish on a row (or chat: finish ENG-123).
14. **Fleet** — Start 2-task run → Run tick (or Run until done). Default
    backend is `stub` (fine). Board columns move; background ticker may also
    tick. With Fleet LLM on, agents talk mid-run (`TEAM_CHAT.md`; chat
    tools `read_team_chat` / `send_team_message`).
15. **Sandboxes** — same worker sessions as Fleet.
16. **Cases** — `/{ws}/cases` New case (title + policy) → Create case. Detail:
    Observe a fact, then Cancel (status cancelled). Empty list before create is OK.
    The LLM invents the workflow; this page is the log.
17. **Inbox** — `/{ws}/inbox` tickets from connected boards. Empty is OK.
    Start monitor follows every connected inbox app (new Connects are picked
    up on the next poll or instantly via the webhook URL on the page).
    Uncheck follow-connected and Save to pin the current watch set.
    Tickets auto-launch work+review; Slack/Teams one ops task;
    Notion/Confluence one analysis task. Finish still starts one by hand.
    A second Finish on the same row must conflict. Closed-on-board while
    launched should mark the row done.
18. **Chat history** — `/{ws}/sessions` lists **chats**, not worker hosts.
19. **User menu** — theme Black/White (not persisted), Log out → `/login`.
20. **This MR** — do the new thing this change added. If the script has no
    step for it, the change is incomplete.

Logs: `./scripts/fleet-dev.sh logs`. Stop: `./scripts/human-test.sh stop`.

---

## Agents: record the demo video

You record. The human reviews the MR (and may replay the walkthrough).

Film the **final tree that will be merged**: every edit for this MR is
already in the worktree, and live identity matches `scripts/tree-id.sh`.
A clip from an earlier bake, or filmed before later edits, is **not**
proof — discard it and re-film. Do not film, then change code, then
attach the old `.webm`.

The video must show **every scene in HUMAN_TEST_SCOPE.md**. It is not a
random walkthrough. The recorder must **use** each feature (click the
primary control, create / cancel / save / tick / invite / pair, then
assert the outcome) and **cross each click with the matching
fleet-server log line** (`method=` + `uri=` + `status=` in
`.run/logs/fleet-server.log`). Opening a page and checking the heading
is not a review. After filming, **review the film** for bugs: every
mid-action frame under `.tmp/demos/<stamp>-frames/` plus the `.webm`.
Fail the review when a control is present but unusable (blank button,
mashed pill, action toast missing, wrong identity) or when the UI
claims success but the log never saw the request. Fill the Film review
section in that file.

```bash
python3 tests/browser/surface_scope.py --write   # after every code change
python3 tests/browser/surface_scope.py --check
./scripts/human-test.sh record                   # films the scope
# then inspect REVIEW_FRAMES + DEMO_VIDEO; fill HUMAN_TEST_SCOPE.md Film review
./scripts/human-test.sh record --list
```

Output: `.tmp/demos/<utc>-<scenes>.webm` (gitignored). Playwright Chromium
must already be installed (`ENABLE_BROWSER_E2E=1` in repo-root `.env`, then
the one install script). Do not pip/apt around a missing browser.

Attach the `.webm` to the GitLab MR (upload / comment). Write one sentence
in the MR description: which affected scenes it shows. Do **not** `git add` videos.

Constraints:

- Use `*@example.test` accounts created in the recording.
- Do not show `.env`, JWT, `ADMIN_TOKEN`, OAuth client secrets, or `~/.grok`.
- Prefer `localhost` in the address bar.
- Keep it short: the affected paths, the new result, stop. Usually 30–90 seconds.
- If UI and API are down after `launch`, **fail**. Do not ship a silent skip.
- Several scenes in one command = one continuous video (better for reviewers).

### Which scene for which code

Pick **every** scene that your diff can break. Omit scenes the merge
does not touch. Do not record `all` “to be safe” unless the change
really spans the product.

| You changed | Scene | The video must show |
| --- | --- | --- |
| `crates/fleet-ui` home, navbar | `landing` | `/` hero, Get started, Log in |
| `crates/fleet-auth`, `fleet-db` users/sessions/workspaces | `auth` | signup → login → `/{uuid}/dashboard`; cookies survive reload |
| `crates/fleet-ui` sidebar, theme, logout | `shell` | expand sections, user menu, theme toggle |
| `crates/fleet-ui` + `fleet-auth` workspaces | `workspace` | switcher + create workspace |
| `crates/fleet-server` `/health`, `/`, `/api` | `api` | browser opens API `/health` JSON (`status: ok`, `apis` includes auth/fleet/chat) |
| `crates/fleet-server` chat, `chat_tools`, `grok` | `chat` | New chat composer + provider/model. Defaults to last used if ready, else first installed. Ollama labeled Private (Ollama). Send only if a provider is live |
| `crates/fleet-db` chats repo | `chat` + `sessions` | new thread appears under Chat history |
| `crates/fleet-orchestrator` tick, plans, store, team chat | `fleet` | Start 2-task run, Run tick, cards change column. LLM workers use TEAM_CHAT.md |
| `crates/fleet-server` `fleet_routes` | `fleet` | same — cookie BFF, not `/v1` |
| `crates/fleet-driver` on a paired `fleet-host` | `fleet` then `sandboxes` | session card, host runtime, work dir. Pair a host before launch |
| `fleet-host` + operator + CRD | `k8s` then `fleet` | CRD + operator + fleet-host present; Start run + tick → host creates a CR |
| `crates/fleet-ui` sandboxes page | `sandboxes` | list or empty hint naming Fleet |
| `crates/fleet-server` `cases.rs`, `fleet-ui` cases | `cases` | New case form; create → Observe → Cancel on detail (empty list OK before create) |
| `crates/fleet-server` `tickets.rs`, `fleet-ui` inbox | `inbox` | list (empty OK before a row) + Start monitor + Finish on a row (launches; Finish hides) + sidebar Inbox under Monitoring |
| `crates/fleet-auth` personas, `fleet-ui` agents | `agents` | Seed starters → eight cards |
| `crates/fleet-integrations` catalog/oauth/proxy | `integrations` | catalog + search. Do not complete real OAuth unless the MR is that provider |
| `crates/fleet-server` + UI settings | `settings` | agent count + Ollama fallback. No git URL. Members: invite by email (member/admin); existing user joins; pending invite; owner remove |
| `crates/fleet-server` `admin.rs`, `fleet-ui` `/admin` | `admin` | Instance page: counts, Users Promote/Demote, Workspaces, all runners, health. Promote via INSTANCE_ADMIN_EMAILS or `scripts/promote-instance-admin.sh`. Non-admin sees Instance admin required |
| unify-runners (isolation, picker, single guest, self-hosted) | `unify` | Settings Isolation save; Self-hosted Add worker; Fleet Start 2-task + tick; Sandboxes. Process-only host must not take a container workspace |
| `crates/fleet-host`, worker-hosts API, UI `/{ws}/runners/self-hosted` | `runners` (`workers` alias) | add worker, paste install command; token panel is success (no second machine). After signup a bootstrap `cluster-host` / local fleet-host may appear. Private (Ollama): top-50, tags, models directory, disk/RAM/VRAM guards. Sandboxing Native / VMs / Containers. `/{ws}/workers` redirects |
| `crates/fleet-ui` Cloud `/{ws}/runners/cloud` | `cloud` | public cloud (AWS / GCP / Azure) + Fleet Cloud placeholders. No provider keys |
| `crates/fleet-ui` Partners `/{ws}/runners/partners` | `partners` | trust copy, disclaimer, verified vs open. Ack does not enable lending |
| `crates/fleet-ui` dashboard + `fleet/overview` + workforce | `dashboard` | Workforce: Set a goal, Open goals / Open runs / Team board (empty OK) |
| `crates/fleet-ui` chat history / live session view | `sessions` | Chat history list; open a fleet session only if one exists |
| `crates/fleet-sandbox-driver` Rust, job JSON, handshake | `lab` | short host protocol run rendered and filmed (no LLM) |
| `sandbox-lab/workspace/workspace_agent.py` | `lab` | same |
| `sandbox-lab/scripts/install-full-suite-deps.sh`, compose, pins | `lab` | install-contract / compose pin output |
| `crates/fleet-operator`, `infra/`, kind | `kind` | only if a kind cluster is up; otherwise say so in the MR and film `api` against the host API you did start |
| Docs-only product wording | — | no video |
| `docs/testing.md` / test harness only | — | no video unless you changed what the human sees |

`all` = landing, auth, shell, workspace, api, chat, fleet, sandboxes, cases, inbox, agents,
integrations, settings, admin, runners, cloud, partners, sessions (logout at the
end of `auth`/`shell` as needed). `workers` is an alias for `runners`.
`lab`, `kind`, and `unify` are extra — pass them when that is the MR.

### Adding a scene (required when you add a surface)

1. Add a function to `tests/browser/record_demo.py` and register it in
   `SCENES`.
2. Add a matching pause-step in `scripts/human-test.sh`.
3. Add a row to the table above.
4. Hit the same locators as `tests/browser/test_app_e2e.py` when the flow
   already exists there.

Rust units and HTTP oneshots stay in Rust (`docs/testing.md`). This recorder
is not a second test framework.

---

## Merge request text (copy)

```
Human testing: HUMAN_TEST_SCOPE.md (not the full catalog).
Demo: agent-recorded .tmp/demos/<file>.webm on the final merge tree;
film review in HUMAN_TEST_SCOPE.md — bugs: <none / list>.
Ask for a review on Slack.
```

Whoever picks it up: watch the clip, then spot-check those same affected
steps on the MR branch if the change is auth, fleet, install, or data.
