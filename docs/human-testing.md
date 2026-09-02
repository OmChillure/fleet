# Human testing and MR demos

Automated tests (`docs/testing.md`) do not replace a person looking at the
product, and they do not produce the **demo video** a merge request needs.

Two roles:

| Who | What they do |
| --- | --- |
| **Human** (author or reviewer) | Run `./scripts/human-test.sh`. It starts Postgres + API + UI and walks every current surface. Confirm the product still makes sense. |
| **Agent** (the coding agent on the MR) | Record the demo video. **Do not ask the human to screen-record.** Use `./scripts/human-test.sh record` (Playwright `.webm`). Attach the file to the GitLab MR. Never commit videos. |

Install is still one script with no flags
(`crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`)
and repo-root `.env`. This page does not install packages.

---

## When this applies

| Change | Human walkthrough | Agent demo video | GitLab MR + peer review |
| --- | --- | --- | --- |
| User-visible UI, API behavior, data, install, tests, sandbox/lab | Yes | Yes — scene for the area you touched | **Required** |
| Operator-only (kind, compose job, workspace agent) | Yes, the relevant step | Yes — `lab` or `kind` scene | **Required** |
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
2. Starts compose Postgres, `fleet-server` (:8080), and `fleet-ui` (:3030)
   via `./scripts/fleet-dev.sh start` if they are not already up.
3. Opens the UI when it can (`xdg-open` / `open`).
4. Pauses on each step below. Press Enter after you have checked the screen.

Subcommands: `launch`, `guide` (no start), `status`, `stop`, `record` (agents).

Use `http://localhost:3030` (not `127.0.0.1`) so cookies match CORS.

**Port-up is not this tree.** Kind often binds host `:8080`/`:3030` to old
`agent-fleet-*:local` images. `human-test.sh` / `record` / browser e2e
require `GET /health` → `git_sha` == `scripts/tree-id.sh`, UI
`/fleet-git-sha.txt` == that id, and `apis` contains `chat`. After code
changes, re-run the install script or rebuild before filming. Existing
`.webm`s filmed without that pair are **not** MR proof — discard them.
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
16. **Cases** — `/{ws}/cases` list (empty is OK). Open a case if chat started one;
    events + Observe / Cancel. The LLM invents the workflow; this page is the log.
17. **Inbox** — `/{ws}/inbox` tickets from connected boards. Empty is OK.
    Start monitor watches Jira/GitHub/Linear/… and auto-launches work+review
    on open tickets (needs an integration). Finish still starts one by hand.
18. **Chat history** — `/{ws}/sessions` lists **chats**, not worker hosts.
19. **User menu** — theme Black/White (not persisted), Log out → `/login`.
20. **This MR** — do the new thing this change added. If the script has no
    step for it, the change is incomplete.

Logs: `./scripts/fleet-dev.sh logs`. Stop: `./scripts/human-test.sh stop`.

---

## Agents: record the demo video

You record. The human reviews the MR (and may replay the walkthrough).

```bash
# Start the stack if needed, then film.
./scripts/human-test.sh record
# Only the areas this MR touches:
./scripts/human-test.sh record fleet sandboxes
./scripts/human-test.sh record --list
```

Output: `.tmp/demos/<utc>-<scenes>.webm` (gitignored). Playwright Chromium
must already be installed (`ENABLE_BROWSER_E2E=1` in repo-root `.env`, then
the one install script). Do not pip/apt around a missing browser.

Attach the `.webm` to the GitLab MR (upload / comment). Write one sentence
in the MR description: what the clip shows. Do **not** `git add` videos.

Constraints:

- Use `*@example.test` accounts created in the recording.
- Do not show `.env`, JWT, `ADMIN_TOKEN`, OAuth client secrets, or `~/.grok`.
- Prefer `localhost` in the address bar.
- Keep it short: one path, the new result, stop. Usually 30–90 seconds.
- If UI and API are down after `launch`, **fail**. Do not ship a silent skip.
- Several scenes in one command = one continuous video (better for reviewers).

### Which scene for which code

Pick every scene that your diff can break. When unsure, record `all`.

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
| `crates/fleet-server` `cases.rs`, `fleet-ui` cases | `cases` | list (empty OK) + sidebar Cases; detail if a case exists |
| `crates/fleet-server` `tickets.rs`, `fleet-ui` inbox | `inbox` | list (empty OK) + sidebar Inbox under Monitoring |
| `crates/fleet-auth` personas, `fleet-ui` agents | `agents` | Seed starters → eight cards |
| `crates/fleet-integrations` catalog/oauth/proxy | `integrations` | catalog + search. Do not complete real OAuth unless the MR is that provider |
| `crates/fleet-server` + UI settings | `settings` | agent count + Ollama fallback. No git URL |
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
integrations, settings, runners, cloud, partners, sessions (logout at the
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
Human testing: ran ./scripts/human-test.sh (or a peer will).
Demo: agent-recorded .tmp/demos/<file>.webm attached — shows <scene>.
Ask for a review on Slack.
```

Whoever picks it up: watch the clip, then spot-check the walkthrough on
the MR branch if the change is auth, fleet, install, or data.
