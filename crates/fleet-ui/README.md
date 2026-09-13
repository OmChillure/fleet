# fleet-ui

Web UI for [Agent Fleet](../../README.md): landing, auth, workspaces, chat, and app shell.

| | |
| --- | --- |
| Stack | [Dioxus](https://dioxuslabs.com/) 0.7 (web) + Tailwind CSS v4 |
| Design doc | [`docs/components/ui/README.md`](../../docs/components/ui/README.md) |
| Workspace crate | `fleet-ui` |
| API base (default) | **Same origin** `/api/*` via `Dioxus.toml` proxy → `:8080` |

---

## Quick start

```bash
# Terminal 1 — backend (workspace root)
cargo run
# → http://127.0.0.1:8080

# Terminal 2 — UI (proxies /api → backend; cookies stay first-party)
# dx comes from the one install script (do not cargo-install it on the host):
#   crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh
dx serve --package fleet-ui --port 3030
# → http://localhost:3030
```

Open **`http://localhost:3030`** (prefer `localhost`, not `127.0.0.1`, for cookie consistency).

```bash
cargo check -p fleet-ui
dx build --package fleet-ui
```

No project `node_modules` is required for `dx serve` / `dx build` (Tailwind is
driven by Dioxus from root `tailwind.css` → `assets/tailwind.css`).

### Auth flow

1. `/signup` → `POST /api/auth/signup` (creates account + default workspace) → `/login`
2. `/login` → `POST /api/auth/login` (httpOnly `fleet_access` + `fleet_refresh`) → dashboard
3. `AppShell` calls `GET /api/auth/me`; if access JWT expired, silent `POST /api/auth/refresh` then retry
4. Only true 401 after refresh fails → redirect `/login` (network errors show Retry)
5. Sidebar loads `GET /api/workspaces`; can `POST /api/workspaces` to create more

API client: `src/pages/auth/api.rs` (`credentials: include`). Dev uses same-origin
`/api` (see `[[web.proxy]]` in `Dioxus.toml`).

Override API origin at build time (split deploy / no proxy):

```bash
FLEET_AUTH_URL=https://api.example.com dx serve --package fleet-ui --port 3030
```

---

## Routes

### Marketing (black landing chrome — no sidebar)

| Path | Feature folder | Notes |
| --- | --- | --- |
| `/` | `src/pages/home/` | Pure black monochrome landing |
| `/login` | `src/pages/auth/login.rs` | Split layout; JWT cookie login |
| `/signup` | `src/pages/auth/signup.rs` | Split layout; then → login |
| `/oauth/done` | `src/pages/integrations/oauth_done.rs` | OAuth popup finish (no shell) |

### App shell (sidebar + workspace switcher)

All app routes are **workspace-scoped**.

| Path | Feature folder | Notes |
| --- | --- | --- |
| `/{workspace_id}/dashboard` | `src/pages/dashboard/` | Workforce goals, approvals, board |
| `/{workspace_id}/chat` | `src/pages/chat/` | Grok / Claude / Ollama / OpenAI / Gemini composer + tools |
| `/{workspace_id}/fleet` | `src/pages/fleet/` | Fleet control plane |
| `/{workspace_id}/sessions` | `src/pages/sessions/` | Chat history + live worker sessions |
| `/{workspace_id}/agents` | `src/pages/agents/` | Personas catalog + seed starters |
| `/{workspace_id}/sandboxes` | `src/pages/sandboxes/` | Live worker sessions on paired hosts |
| `/{workspace_id}/inbox` | `src/pages/inbox/` | Inbox apps index |
| `/{workspace_id}/inbox/:provider` | `src/pages/inbox/app.rs` | One app's tickets + filters |
| `/{workspace_id}/cases` | `src/pages/cases/` | Durable cases |
| `/{workspace_id}/integrations` | `src/pages/integrations/` | OAuth catalog + connect |
| `/{workspace_id}/settings` | `src/pages/settings/` | Concurrent agents + members + min isolation |
| `/{workspace_id}/runners/self-hosted` | `src/pages/runners/self_hosted/` | Pair + list worker hosts |
| `/{workspace_id}/runners/cloud` | `src/pages/runners/cloud/` | Fleet Cloud / BYO attach |
| `/{workspace_id}/runners/partners` | `src/pages/runners/partners/` | Partner lend / borrow |
| `/admin` | `src/pages/admin/` | Instance admin (users, runners, health) |

Routes are defined in `src/main.rs`. Folder names organize code only.

`Route::workspace_id()` returns the active id for shell chrome (sidebar links,
workspace dropdown).

---

## Source layout

```text
src/
  main.rs                 # Route enum, MarketingLayout, App + ToastProvider
  theme.rs                # Theme::Black | Theme::White
  components/
    app_shell.rs          # Auth gate + sidebar + theme provider
    navbar.rs             # Marketing navbar
    sidebar.rs            # Workspace dropdown + nav sections
    toast.rs              # Global bottom-right toasts (show_toast())
  pages/
    auth/
      login.rs / signup.rs / layout.rs
      api.rs              # login, me, workspaces client
    chat/                 # /{ws}/chat
    dashboard/ fleet/ sessions/ agents/ sandboxes/
    integrations/         # catalog UI + oauth_done
    settings/
    home/
    shared/               # EmptyPage, etc.
tailwind.css              # Tailwind v4 entry (dx auto-detect)
assets/tailwind.css       # Generated output (served via asset!)
```

### Feature folder convention

| File | Role |
| --- | --- |
| `page.rs` or named screen (`login.rs`) | UI component |
| `api.rs` | Client-side fetch/types for this feature |
| `mod.rs` | Module exports |

---

## Layouts, theme, toasts

| Layout | Used by | Chrome |
| --- | --- | --- |
| `MarketingLayout` | `/`, `/login`, `/signup` | Navbar + footer; fixed black |
| `AppShell` | `/{workspace_id}/*` | Auth gate, sidebar, full-height content |
| `ToastProvider` | Entire app (`main.rs`) | Global toasts (outside theme vars) |

App theme CSS variables: `.theme-black` / `.theme-white` in `tailwind.css`
(default black; appearance can still be toggled from the user menu if present).

### Toasts

```rust
use crate::components::toast::show_toast;

show_toast().info("Authorize Slack in the new tab…");
show_toast().success("slack connected.");
show_toast().error("Something failed.");
```

- Bottom-right, square corners, solid high-contrast colors
- Auto-dismiss ~4s + **OK** pill
- Used for auth validation/API errors and integration connect/disconnect

### Sidebar

- **Workspace dropdown** above nav: switch workspace, create new
- Sections: **Chat** (New chat, Chat history), **Monitoring**, **Agents**, **Workspace**
- Create uses `POST /api/workspaces` then navigates to the new dashboard

### Chat

- Route: `/{workspace_id}/chat` (sidebar **New chat**)
- Full-height message list + floating bottom composer (Enter to send)
- Chat turns persist via `/api/workspaces/{id}/chat` (and `/stream`)

### Integrations UI

- Catalog from `GET /api/workspaces/{id}/integrations`
- **Connect** opens OAuth in a new tab; card flips to Connected when auth finishes
  (postMessage + localStorage + API poll; optimistic UI)
- Tokens never appear in the browser — only connection metadata

---

## Styling (Tailwind)

Use utilities on elements:

```rust
div { class: "flex min-h-screen flex-col gap-4",
    // …
}
```

Dioxus 0.7 runs Tailwind when it finds root `tailwind.css` (`dx serve` /
`dx build`). Output: `assets/tailwind.css`.

---

## Status

| Area | Status |
| --- | --- |
| Routing + feature folders | Done |
| Marketing landing (black) | Done |
| App shell, sidebar, B/W theme | Done |
| Login + signup UI + toasts | Done |
| Real auth / Postgres (`fleet-auth`) | Done (JWT cookies) |
| Default workspace on signup/login | Done |
| Workspace-scoped routes + switcher | Done |
| App shell session gate | Done |
| Chat page (composer UI) | Done (no backend yet) |
| Integrations OAuth UI | Done |
| Global toasts | Done |
| Live fleet / orchestrator data | Partial (fleet page) |

---

## Related

- Component design: [`docs/components/ui/`](../../docs/components/ui/)
- Auth design: [`docs/components/auth/`](../../docs/components/auth/)
- Integrations: [`docs/components/integrations/`](../../docs/components/integrations/)
- API library: [`crates/fleet-auth/`](../fleet-auth/)
- Integrations crate: [`crates/fleet-integrations/`](../fleet-integrations/)
- Architecture: [`docs/architecture/README.md`](../../docs/architecture/README.md)
