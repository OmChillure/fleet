# fleet-ui

Web UI for [Agent Fleet](../../README.md): landing, auth, workspaces, and app shell.

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
# Requires: dioxus-cli 0.7.x matching dioxus = 0.7.3
dx serve --package fleet-ui --port 3030
# → http://localhost:3030
```

Open **`http://localhost:3030`** (prefer `localhost`, not `127.0.0.1`, for cookie consistency).

```bash
cargo check -p fleet-ui
dx build --package fleet-ui
```

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

### Marketing (black landing chrome — no sidebar / theme toggle)

| Path | Feature folder | Notes |
| --- | --- | --- |
| `/` | `src/pages/home/` | Pure black monochrome landing |
| `/login` | `src/pages/auth/login.rs` | Split layout; JWT cookie login |
| `/signup` | `src/pages/auth/signup.rs` | Split layout; then → login |

### App shell (sidebar + workspace switcher + B/W theme)

All app routes are **workspace-scoped**.

| Path | Feature folder | Notes |
| --- | --- | --- |
| `/{workspace_id}/dashboard` | `src/pages/dashboard/` | Empty placeholder |
| `/{workspace_id}/sessions` | `src/pages/sessions/` | Empty placeholder |
| `/{workspace_id}/agents` | `src/pages/agents/` | Empty placeholder |
| `/{workspace_id}/sandboxes` | `src/pages/sandboxes/` | Empty placeholder |
| `/{workspace_id}/integrations` | `src/pages/integrations/` | Catalog from `GET /api/integrations` |
| `/{workspace_id}/settings` | `src/pages/settings/` | Empty placeholder |

Routes are defined in `src/main.rs`. Folder names organize code only.

`Route::workspace_id()` returns the active id for shell chrome (sidebar links,
workspace dropdown).

---

## Source layout

```text
src/
  main.rs                 # Route enum, MarketingLayout, App
  theme.rs                # Theme::Black | Theme::White
  components/
    app_shell.rs          # Auth gate + sidebar + theme provider
    navbar.rs             # Marketing navbar
    sidebar.rs            # Workspace dropdown + nav links
    theme_toggle.rs
  pages/
    auth/
      login.rs / signup.rs / layout.rs
      api.rs              # login, me, workspaces client
    dashboard/ sessions/ agents/ sandboxes/ settings/
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

## Layouts & theme

| Layout | Used by | Chrome |
| --- | --- | --- |
| `MarketingLayout` | `/`, `/login`, `/signup` | Navbar + footer; fixed black |
| `AppShell` | `/{workspace_id}/*` | Auth gate, sidebar (workspace switcher), theme toggle |

App theme CSS variables: `.theme-black` / `.theme-white` in `tailwind.css`.
Toggle is top-right on app pages only.

### Sidebar

- **Workspace dropdown** above Dashboard: switch workspace, create new
- Nav links rebuild with the current `workspace_id`
- Create uses `POST /api/workspaces` then navigates to the new dashboard

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
| Login + signup UI | Done |
| Real auth / Postgres (`fleet-auth`) | Done (JWT cookies) |
| Default workspace on signup/login | Done |
| Workspace-scoped routes + switcher | Done |
| App shell session gate | Done |
| Live fleet / orchestrator data | Not started |

---

## Related

- Component design: [`docs/components/ui/`](../../docs/components/ui/)
- Auth design: [`docs/components/auth/`](../../docs/components/auth/)
- API library: [`crates/fleet-auth/`](../fleet-auth/)
- Contracts: [`crates/agent-fleet-contracts/`](../agent-fleet-contracts/)
- Architecture: [`docs/architecture/README.md`](../../docs/architecture/README.md)
