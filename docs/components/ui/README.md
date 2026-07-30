# fleet-ui (Web UI)

Primary web interface for Agent Fleet: marketing landing, auth screens, and the
app shell (dashboard and related views).

**Crate:** [`crates/fleet-ui/`](../../../crates/fleet-ui/)  
**Stack:** [Dioxus](https://dioxuslabs.com/) 0.7 (Rust → WASM) + Tailwind CSS v4  
**Developer guide:** [`crates/fleet-ui/README.md`](../../../crates/fleet-ui/README.md)

---

## 1. Role in the system

| Concern | Owner |
| --- | --- |
| Browser UI, routing, theming | **fleet-ui** |
| Auth HTTP / users / sessions (planned) | `fleet-auth` (not implemented yet) |
| Control-plane commands & events | [`agent-fleet-contracts`](../../../crates/agent-fleet-contracts/) + orchestrator |
| Durable multi-agent state | Postgres (planned; self-host Docker or managed) |

Today the UI is a **front-end shell** with mock/demo flows. It does not yet call
a backend API. Feature folders include empty `api.rs` modules ready for client
fetch helpers.

```
  Browser (fleet-ui)
       │  HTTPS / WS  (planned)
       ▼
  API + fleet-auth  →  Postgres
       │
       ▼
  Orchestrator / events  (Zenoh, etc.)
```

Product vision: **primary interface is Web** (shared URL, no desktop install
required for most users). CLI/TUI remain optional later.

---

## 2. Surface map

Two chrome modes:

### Marketing (no app sidebar, no theme toggle)

| Path | Purpose |
| --- | --- |
| `/` | Landing — pure black monochrome |
| `/login` | Demo login (any non-empty email + password → dashboard) |

### App shell (sidebar + black/white theme toggle)

| Path | Purpose | UI status |
| --- | --- | --- |
| `/dashboard` | Fleet overview | Empty placeholder |
| `/sessions` | Agent sessions / streams | Empty placeholder |
| `/agents` | Roles, models, budgets | Empty placeholder |
| `/sandboxes` | Sandbox health / resources | Empty placeholder |
| `/settings` | Workspace / integrations | Empty placeholder |

```
Marketing                          App shell
┌─────────────────────┐            ┌────────┬──────────────────────┐
│ Navbar              │            │ Side   │ Theme [Black|White]  │
│ page                │            │ bar    ├──────────────────────┤
│ footer              │            │        │ feature page         │
└─────────────────────┘            └────────┴──────────────────────┘
```

---

## 3. Code layout (feature folders)

URLs are **flat** (`/dashboard`). Source is **feature-based** for visibility:

```text
crates/fleet-ui/src/
  main.rs                 # Route enum, MarketingLayout, App entry
  theme.rs                # Black / White app theme signal
  components/
    app_shell.rs          # Sidebar + top bar + theme context
    navbar.rs             # Marketing navbar
    sidebar.rs
    theme_toggle.rs
  pages/
    home/                 # /
    auth/                 # /login (+ signup later under same folder)
      login.rs
      api.rs
    dashboard/            # /dashboard
      page.rs             # UI
      api.rs              # client helpers for this feature (stubs)
    sessions/
    agents/
    sandboxes/
    settings/
    shared/               # EmptyPage, cross-page widgets
```

**Convention**

| File | Responsibility |
| --- | --- |
| `page.rs` (or `login.rs`) | RSX / page component |
| `api.rs` | Browser-side calls for **this** feature only |
| `mod.rs` | `mod page; mod api; pub use …` |

Auth screens live under `pages/auth/` (not a separate `signup/` feature).  
When signup is added: `pages/auth/signup.rs` + route `/signup`.

Routes are declared only in `main.rs` via Dioxus `Routable` — folder names
do not change the URL.

---

## 4. Theming

| Area | Theme behavior |
| --- | --- |
| Landing + login | Fixed **black** monochrome (no purple/blue accents) |
| App shell | **Black** or **White** via CSS variables (`.theme-black` / `.theme-white`) |
| Toggle | Top-right of app shell only — not on marketing pages |

Theme state is an in-memory Dioxus signal provided by `AppShell` (not persisted
yet).

---

## 5. Styling

- **Tailwind utility classes** on RSX elements (`class: "flex gap-2 …"`), same
  idea as React `className`.
- Dioxus 0.7 **auto Tailwind**: root `tailwind.css` is detected by `dx`; output
  is `assets/tailwind.css`, loaded via `document::Stylesheet` + `asset!()`.
- No project `node_modules` required when using `dx serve` / `dx build`.

See [Dioxus styling / Tailwind](https://dioxuslabs.com/learn/0.7/essentials/ui/styling/).

---

## 6. Run / develop

Requirements:

- Rust toolchain (workspace `rust-version`)
- [Dioxus CLI](https://dioxuslabs.com/) matching crate (`dioxus = 0.7.3` pinned)

```bash
# from workspace root (agent-fleet/)
dx serve --package fleet-ui
```

Open the printed URL (typically `http://127.0.0.1:8080`).

```bash
cargo check -p fleet-ui
dx build --package fleet-ui
```

---

## 7. Current limitations / next steps

| Status | Item |
| --- | --- |
| Done | Routes, app shell, feature folders, B/W theme |
| Done | Login + signup split UI; JWT cookies via [`fleet-auth`](../../../crates/fleet-auth/) |
| Not done | Live data from orchestrator / contracts |
| Not done | Route guards / session bootstrap on app shell |
| Not done | Theme persistence (localStorage) |
| Not done | Route guards (require session for app shell) |

Suggested order later: **auth backend → wire `pages/auth/api.rs` → guard app
routes → dashboard data via API**.

---

## 8. Related docs

| Doc | Why |
| --- | --- |
| [`crates/fleet-ui/README.md`](../../../crates/fleet-ui/README.md) | Day-to-day develop / run |
| [`docs/architecture/README.md`](../../architecture/README.md) | Control plane layers |
| [`docs/product_vision.md`](../../product_vision.md) | Web as primary interface |
| [`docs/architecture/events.md`](../../architecture/events.md) | Event shapes UI may eventually stream |
