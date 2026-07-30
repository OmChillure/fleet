# fleet-ui

Web UI for [Agent Fleet](../../README.md): landing, auth, and app shell.

| | |
| --- | --- |
| Stack | [Dioxus](https://dioxuslabs.com/) 0.7 (web) + Tailwind CSS v4 |
| Design doc | [`docs/components/fleet-ui/README.md`](../../docs/components/fleet-ui/README.md) |
| Workspace crate | `fleet-ui` |

---

## Quick start

```bash
# From workspace root (agent-fleet/)
# Requires: dioxus-cli 0.7.x matching dioxus = 0.7.3
# Backend on :8080 must already be running (`cargo run`)
dx serve --package fleet-ui --port 3030
```

Open `http://localhost:3030`. API: `http://localhost:8080`.

```bash
cargo check -p fleet-ui
dx build --package fleet-ui
```

**Auth:** `/signup` creates an account, then redirects to `/login`.
Login sets httpOnly JWT cookies and routes to `/{workspace_id}/dashboard`.

Requires `fleet-server` (`cargo run` from workspace root) and `DATABASE_URL` in `.env`.

---

## Routes

### Marketing (black landing chrome — no sidebar / theme toggle)

| Path | Feature folder | Notes |
| --- | --- | --- |
| `/` | `src/pages/home/` | Pure black monochrome landing |
| `/login` | `src/pages/auth/login.rs` | Split layout; JWT cookie login |
| `/signup` | `src/pages/auth/signup.rs` | Split layout; then → login |

### App shell (sidebar + black/white theme toggle)

| Path | Feature folder | Notes |
| --- | --- | --- |
| `/dashboard` | `src/pages/dashboard/` | Empty placeholder |
| `/sessions` | `src/pages/sessions/` | Empty placeholder |
| `/agents` | `src/pages/agents/` | Empty placeholder |
| `/sandboxes` | `src/pages/sandboxes/` | Empty placeholder |
| `/settings` | `src/pages/settings/` | Empty placeholder |

Routes are defined in `src/main.rs`. Folder names organize code only; they do
not change URL paths.

---

## Source layout

```text
src/
  main.rs                 # Route, MarketingLayout, App
  theme.rs                # Theme::Black | Theme::White
  components/             # Shared chrome
    app_shell.rs          # App layout + theme provider
    navbar.rs             # Marketing navbar
    sidebar.rs
    theme_toggle.rs
  pages/
    auth/                 # All auth UI lives here
      login.rs            # /login
      api.rs              # login/signup client helpers (stubs)
      # signup.rs         # add later → /signup
    dashboard/
      page.rs             # UI
      api.rs              # feature client API (stubs)
      mod.rs
    sessions/  agents/  sandboxes/  settings/
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

Auth is **`pages/auth/`**, not a separate `login` feature folder. Future signup
belongs in the same folder.

---

## Layouts & theme

| Layout | Used by | Chrome |
| --- | --- | --- |
| `MarketingLayout` | `/`, `/login` | Navbar + footer; fixed black |
| `AppShell` | dashboard and other app routes | Sidebar + top bar; B/W theme |

App theme CSS variables: `.theme-black` / `.theme-white` in `tailwind.css`.
Toggle is top-right on app pages only.

---

## Styling (Tailwind)

Use utilities on elements (React-style):

```rust
div { class: "flex min-h-screen flex-col gap-4",
    // …
}
```

Dioxus 0.7 runs Tailwind when it finds root `tailwind.css` (`dx serve` /
`dx build`). Output: `assets/tailwind.css`.

No npm/`node_modules` required for the normal `dx` workflow.

---

## Status

| Area | Status |
| --- | --- |
| Routing + feature folders | Done |
| Marketing landing (black) | Done |
| App shell, sidebar, B/W theme | Done |
| Login + signup UI (split layout) | Done |
| Real auth / Postgres (`fleet-auth`) | Done (JWT cookies) |
| Live fleet data | Not started |

---

## Related

- Component design: [`docs/components/fleet-ui/`](../../docs/components/fleet-ui/)
- Contracts: [`crates/agent-fleet-contracts/`](../agent-fleet-contracts/)
- Architecture: [`docs/architecture/README.md`](../../docs/architecture/README.md)
