# fleet-ui (Web UI)

Primary web interface for Agent Fleet: marketing landing, auth screens, workspace
switcher, and the app shell (dashboard and related views).

**Crate:** [`crates/fleet-ui/`](../../../crates/fleet-ui/)  
**Stack:** [Dioxus](https://dioxuslabs.com/) 0.7 (Rust → WASM) + Tailwind CSS v4  
**Developer guide:** [`crates/fleet-ui/README.md`](../../../crates/fleet-ui/README.md)  
**Auth / workspaces:** [`docs/components/auth/README.md`](../auth/README.md)

---

## 1. Role in the system

| Concern | Owner |
| --- | --- |
| Browser UI, routing, theming | **fleet-ui** |
| Auth HTTP / users / sessions / workspaces | [`fleet-auth`](../../../crates/fleet-auth/) via [`fleet-server`](../../../crates/fleet-server/) |
| Control-plane commands & events | [`agent-fleet-contracts`](../../../crates/agent-fleet-contracts/) + orchestrator (future) |
| Durable state | Postgres via [`fleet-db`](../../../crates/fleet-db/) |

```text
  Browser (fleet-ui :3030)
       │  cookie credentials → http://localhost:8080
       ▼
  fleet-server  →  fleet-auth  →  fleet-db / Postgres
       │
       ▼
  Orchestrator / events  (planned)
```

Product vision: **primary interface is Web**. CLI/TUI remain optional later.

---

## 2. Surface map

### Marketing (no app sidebar, no theme toggle)

| Path | Purpose |
| --- | --- |
| `/` | Landing — pure black monochrome |
| `/login` | Real login → cookies + `/{workspace_id}/dashboard` |
| `/signup` | Create account + default workspace → `/login` |

### App shell (auth-gated; sidebar + workspace switcher + theme)

| Path | Purpose | UI status |
| --- | --- | --- |
| `/{workspace_id}/dashboard` | Fleet overview | Empty placeholder |
| `/{workspace_id}/sessions` | Agent sessions / streams | Empty placeholder |
| `/{workspace_id}/agents` | Roles, models, budgets | Empty placeholder |
| `/{workspace_id}/sandboxes` | Sandbox health / resources | Empty placeholder |
| `/{workspace_id}/integrations` | Connect GitHub, Jira, Slack, … | Catalog cards from API |
| `/{workspace_id}/settings` | Workspace preferences | Empty placeholder |

```text
Marketing                          App shell
┌─────────────────────┐            ┌────────┬──────────────────────┐
│ Navbar              │            │ Side   │ Theme [Black|White]  │
│ page                │            │ bar    ├──────────────────────┤
│ footer              │            │ WS ▾   │ feature page         │
└─────────────────────┘            │ nav…   │                      │
                                   └────────┴──────────────────────┘
```

**Workspace dropdown** (above Dashboard): list memberships, switch
(`→ /{id}/dashboard`), or create a workspace.

**Auth gate:** `AppShell` calls `GET /api/auth/me` on mount; failure → `/login`.

---

## 3. Code layout (feature folders)

URLs are **workspace-scoped** for app pages. Source is **feature-based**:

```text
crates/fleet-ui/src/
  main.rs                 # Route enum, MarketingLayout, App entry
  theme.rs                # Black / White app theme signal
  components/
    app_shell.rs          # Auth gate + sidebar + top bar + theme
    navbar.rs             # Marketing navbar
    sidebar.rs            # Workspace dropdown + nav
    theme_toggle.rs
  pages/
    home/                 # /
    auth/                 # /login, /signup
      login.rs signup.rs layout.rs
      api.rs              # fleet-server client (cookies + workspaces)
    dashboard/            # /{workspace_id}/dashboard
    sessions/ agents/ sandboxes/ integrations/ settings/
    shared/               # EmptyPage, cross-page widgets
```

**Convention**

| File | Responsibility |
| --- | --- |
| `page.rs` (or `login.rs`) | RSX / page component |
| `api.rs` | Browser-side calls for **this** feature (auth/workspaces shared under `auth/api.rs`) |
| `mod.rs` | `mod page; mod api; pub use …` |

Routes are declared only in `main.rs` via Dioxus `Routable`.

---

## 4. API client

Default base URL: **`http://localhost:8080`**  
(`option_env!("FLEET_AUTH_URL")` override at build time.)

| Client helper | Backend |
| --- | --- |
| `signup` / `login` / `logout` / `me` | `/api/auth/*` |
| `list_workspaces` / `create_workspace` | `/api/workspaces` |

All requests use **credentials include** so httpOnly cookies are stored and sent
cross-origin (UI `:3030` → API `:8080`). CORS must allow the UI origin.

---

## 5. Theming

| Area | Theme behavior |
| --- | --- |
| Landing + auth | Fixed **black** monochrome |
| App shell | **Black** or **White** via CSS variables (`.theme-black` / `.theme-white`) |
| Toggle | Top-right of app shell only |

Theme state is an in-memory Dioxus signal from `AppShell` (not persisted yet).

---

## 6. Styling

- **Tailwind utility classes** on RSX elements (`class: "flex gap-2 …"`).
- Dioxus 0.7 **auto Tailwind**: root `tailwind.css` → `assets/tailwind.css`.
- No project `node_modules` required for `dx serve` / `dx build`.

---

## 7. Run / develop

Requirements:

- Rust toolchain (workspace `rust-version`)
- [Dioxus CLI](https://dioxuslabs.com/) matching crate (`dioxus = 0.7.3` pinned)
- Running `fleet-server` with `DATABASE_URL` + `JWT_SECRET`

```bash
# terminal 1 — from agent-fleet/
cargo run
# → http://localhost:8080

# terminal 2
dx serve --package fleet-ui --port 3030
# → http://localhost:3030
```

Use **`http://localhost:3030`** in the browser (match `CORS_ORIGINS`).

```bash
cargo check -p fleet-ui
dx build --package fleet-ui
```

---

## 8. Current status / next steps

| Status | Item |
| --- | --- |
| Done | Routes, app shell, feature folders, B/W theme |
| Done | Login + signup; JWT cookies via fleet-auth |
| Done | Default workspace; list/create; workspace-scoped URLs |
| Done | App shell session gate (`/api/auth/me`) |
| Not done | Live orchestrator / session / sandbox data |
| Not done | Theme persistence (localStorage) |
| Not done | Workspace settings / invites UI |

Suggested order later: **dashboard data via API → sessions streams → sandbox
health → workspace settings**.

---

## 9. Related docs

| Doc | Why |
| --- | --- |
| [`crates/fleet-ui/README.md`](../../../crates/fleet-ui/README.md) | Day-to-day develop / run |
| [`docs/components/auth/README.md`](../auth/README.md) | Cookies, workspace API, CORS |
| [`docs/architecture/README.md`](../../architecture/README.md) | Control plane layers |
| [`docs/product_vision.md`](../../product_vision.md) | Web as primary interface |
| [`docs/architecture/events.md`](../../architecture/events.md) | Event shapes UI may eventually stream |
