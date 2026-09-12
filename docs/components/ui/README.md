# fleet-ui (Web UI)

Primary web interface for Agent Fleet: marketing landing, auth screens, workspace
switcher, chat, integrations, and the app shell.

**Crate:** [`crates/fleet-ui/`](../../../crates/fleet-ui/)  
**Stack:** [Dioxus](https://dioxuslabs.com/) 0.7 (Rust → WASM) + Tailwind CSS v4  
**Developer guide:** [`crates/fleet-ui/README.md`](../../../crates/fleet-ui/README.md)  
**Auth / workspaces:** [`docs/components/auth/README.md`](../auth/README.md)  
**Integrations:** [`docs/components/integrations/README.md`](../integrations/README.md)

---

## 1. Role in the system

| Concern | Owner |
| --- | --- |
| Browser UI, routing, theming, toasts | **fleet-ui** |
| Auth HTTP / users / sessions / workspaces | [`fleet-auth`](../../../crates/fleet-auth/) via [`fleet-server`](../../../crates/fleet-server/) |
| OAuth connect + encrypted tokens | [`fleet-integrations`](../../../crates/fleet-integrations/) |
| Control-plane commands & events | [`agent-fleet-contracts`](../../../crates/agent-fleet-contracts/) + orchestrator |
| Durable state | Postgres via [`fleet-db`](../../../crates/fleet-db/) |

```text
  Browser (fleet-ui :3030)
       │  cookie credentials → same-origin /api proxy → :8080
       ▼
  fleet-server  →  fleet-auth / fleet-integrations  →  fleet-db / Postgres
       │
       ▼
  Orchestrator / events
```

Product vision: **primary interface is Web**. CLI/TUI remain optional later.

---

## 2. Surface map

### Marketing (no app sidebar)

| Path | Purpose |
| --- | --- |
| `/` | Landing — pure black monochrome |
| `/login` | Real login → cookies + `/{workspace_id}/dashboard` |
| `/signup` | Create account + default workspace → `/login` |
| `/oauth/done` | OAuth popup only — signals opener and closes |

### App shell (auth-gated; sidebar + workspace switcher)

| Path | Purpose | UI status |
| --- | --- | --- |
| `/{workspace_id}/dashboard` | Fleet overview | Live team board + chat |
| `/{workspace_id}/chat` | Talk to the fleet | Composer + local messages |
| `/{workspace_id}/fleet` | Orchestrator control plane | Live control UI |
| `/{workspace_id}/sessions` | Chat history / sessions | Empty placeholder |
| `/{workspace_id}/agents` | Roles, models, budgets | Empty placeholder |
| `/{workspace_id}/sandboxes` | Sandbox health / resources | Empty placeholder |
| `/{workspace_id}/cases` | Durable cases | List + detail, observe, cancel |
| `/{workspace_id}/integrations` | Connect platforms via OAuth | Catalog + connect flow |
| `/admin` | Instance admin | Users, all runners, health (instance_admin only) |
| `/{workspace_id}/settings` | Workspace preferences | Concurrent agents + local fleet LLM (no git URL) |
| `/{workspace_id}/workers` | Self-hosted workers | Add worker + one-time pair token |

```text
Marketing                          App shell
┌─────────────────────┐            ┌────────┬──────────────────────┐
│ Navbar              │            │ Side   │                      │
│ page                │            │ bar    │  feature page        │
│ footer              │            │ WS ▾   │  (full height)       │
└─────────────────────┘            │ Chat…  │                      │
                                   └────────┴──────────────────────┘
                                              toast (bottom-right)
```

**Workspace dropdown:** list memberships, switch (`→ /{id}/dashboard`), or create.

**Auth gate:** `AppShell` calls `GET /api/auth/me` on mount; unauthorized → `/login`.

**Sidebar sections:** Chat · Monitoring · Agents · Workspace.

---

## 3. Code layout (feature folders)

```text
crates/fleet-ui/src/
  main.rs                 # Route enum, MarketingLayout, ToastProvider, App
  theme.rs                # Black / White app theme signal
  components/
    app_shell.rs          # Auth gate + sidebar + theme
    navbar.rs             # Marketing navbar
    sidebar.rs            # Workspace dropdown + nav
    toast.rs              # show_toast().info|success|error
  pages/
    home/                 # /
    auth/                 # /login, /signup
    chat/                 # /{ws}/chat
    dashboard/ fleet/ sessions/ agents/ sandboxes/
    integrations/         # catalog + oauth_done
    settings/
    workers/              # /{ws}/workers (paired hosts)
    shared/               # EmptyPage
```

**Convention**

| File | Responsibility |
| --- | --- |
| `page.rs` (or `login.rs`) | RSX / page component |
| `api.rs` | Browser-side calls for **this** feature |
| `mod.rs` | `mod page; mod api; pub use …` |

Routes are declared only in `main.rs` via Dioxus `Routable`.

---

## 4. API client

Default base URL: **page origin** (dev proxy) or `option_env!("FLEET_AUTH_URL")`.

| Client helper | Backend |
| --- | --- |
| `signup` / `login` / `logout` / `me` | `/api/auth/*` |
| `list_workspaces` / `create_workspace` | `/api/workspaces` |
| `list_integrations` / `disconnect_integration` | `/api/workspaces/{id}/integrations…` |

All requests use **credentials include** so httpOnly cookies are stored and sent.

---

## 5. Theming & toasts

| Area | Behavior |
| --- | --- |
| Landing + auth | Fixed **black** monochrome |
| App shell | **Black** (default) or **White** via `.theme-black` / `.theme-white` |
| Toasts | Solid zinc palette (host is outside AppShell — no CSS vars) |

```rust
show_toast().info("…");
show_toast().success("…");
show_toast().error("…");
```

---

## 6. Styling

- **Tailwind utility classes** on RSX elements.
- Dioxus 0.7 **auto Tailwind**: root `tailwind.css` → `assets/tailwind.css`.
- No project `node_modules` required for `dx serve` / `dx build`.

---

## 7. Run / develop

```bash
# terminal 1 — from agent-fleet/
cargo run
# → http://localhost:8080

# terminal 2
dx serve --package fleet-ui --port 3030
# → http://localhost:3030
```

Use **`http://localhost:3030`** in the browser (match `CORS_ORIGINS`).

---

## 8. Current status / next steps

| Status | Item |
| --- | --- |
| Done | Routes, app shell, feature folders |
| Done | Login + signup; JWT cookies; toasts |
| Done | Default workspace; list/create; workspace-scoped URLs |
| Done | Chat page UI (local placeholder replies) |
| Done | Integrations OAuth UI + connect refresh |
| Partial | Fleet page control plane |
| Not done | Chat backend / streaming |
| Not done | Live dashboard / sessions streams |
| Not done | Theme persistence (localStorage) |

---

## 9. Related docs

| Doc | Why |
| --- | --- |
| [`crates/fleet-ui/README.md`](../../../crates/fleet-ui/README.md) | Day-to-day develop / run |
| [`docs/components/auth/README.md`](../auth/README.md) | Cookies, workspace API, CORS |
| [`docs/components/integrations/README.md`](../integrations/README.md) | OAuth + encryption |
| [`docs/architecture/README.md`](../../architecture/README.md) | Control plane layers |
| [`docs/product_vision.md`](../../product_vision.md) | Web as primary interface |
