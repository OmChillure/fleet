# Auth & workspaces

Identity, sessions, and multi-tenant workspaces for Agent Fleet.

| | |
| --- | --- |
| Library | [`crates/fleet-auth/`](../../../crates/fleet-auth/) |
| Mounted by | [`crates/fleet-server/`](../../../crates/fleet-server/) |
| Persistence | [`crates/fleet-db/`](../../../crates/fleet-db/) |
| UI client | [`crates/fleet-ui/src/pages/auth/api.rs`](../../../crates/fleet-ui/src/pages/auth/api.rs) |

---

## 1. Role in the system

| Concern | Owner |
| --- | --- |
| Signup / login / JWT cookies | **fleet-auth** |
| Users, auth sessions, workspaces tables | **fleet-db** |
| HTTP process, CORS, bind | **fleet-server** |
| Browser forms + cookie credentials | **fleet-ui** |

```text
  fleet-ui (:3030)
       │  POST/GET /api/*  (credentials: include)
       ▼
  fleet-server (:8080)
       ├── fleet-auth routes
       └── fleet-db → Postgres (users, auth_sessions, workspaces, …)
```

---

## 2. Session model

1. **Login** inserts an `auth_sessions` row (refresh token hash + family id).
2. **Access JWT** (`fleet_access` cookie) carries `sub` (user id), `email`, `sid` (session id).
3. **Refresh** (`fleet_refresh` cookie) is opaque; hashed with SHA-256 in DB.
4. Refresh rotation revokes the previous row and inserts a new one in the same family.
5. Reuse of a revoked refresh token revokes the whole family (theft detection).

Handlers that need a user call `require_user` (decode JWT + check session still active).

---

## 3. Workspaces

Workspaces are the multi-tenant boundary. Users may belong to many; each membership has a role (`owner` / `admin` / `member`).

### Default workspace

Created automatically when missing:

| Trigger | Behavior |
| --- | --- |
| Signup | Always create personal workspace in the same transaction as the user |
| Login / refresh | `ensure_default_workspace` if the user has zero memberships |
| `GET /api/auth/me` | Same ensure |
| `GET /api/workspaces` | Same ensure |

**Name:** `{display_name}'s workspace` or `{email-local}'s workspace`  
**Slug:** email-derived prefix + short user id (unique)

### API

| Method | Path | Body / response |
| --- | --- | --- |
| `GET` | `/api/workspaces` | `{ "workspaces": [ { id, slug, name, role } ] }` |
| `POST` | `/api/workspaces` | Body `{ "name", "slug"? }` → `{ "workspace": { … } }` (201) |

Login / refresh also return `default_workspace` so the UI can navigate without an extra round trip.

---

## 4. Local ports & CORS

| Service | Default |
| --- | --- |
| UI | `http://localhost:3030` |
| API | `http://localhost:8080` |

`CORS_ORIGINS` must list the UI origin(s). Credentials are required both sides
(`allow_credentials` + browser `credentials: include`).

Prefer a single hostname in the browser (`localhost` **or** `127.0.0.1`, not mixed)
so cookies set by the API match subsequent requests.

---

## 5. Env (auth-related)

| Variable | Purpose |
| --- | --- |
| `JWT_SECRET` | HMAC secret (≥ 32 chars) |
| `JWT_ACCESS_TTL_SECS` | Access cookie / JWT lifetime |
| `JWT_REFRESH_TTL_SECS` | Refresh session lifetime |
| `COOKIE_SECURE` | `Secure` flag on cookies |
| `COOKIE_SAMESITE` | `Lax` (default) / `Strict` / `None` |
| `CORS_ORIGINS` | Allowed UI origins |
| `INSTANCE_ADMIN_EMAILS` | Comma-separated emails granted instance admin on login |

Full table: [`.env.example`](../../../.env.example), [`fleet-server/README.md`](../../../crates/fleet-server/README.md).

---

## 6. Status

| Item | Status |
| --- | --- |
| Signup / login / logout / refresh / me | Done |
| Default personal workspace | Done |
| List + create workspaces | Done |
| Password hashing (argon2) | Done |
| Workspace roles beyond owner on create | Schema ready; UI only creates as owner |
| OAuth / SSO / invites | Invites done; SSO not started |
| Email verification | Not started |
| Instance admin (`users.instance_admin`) | Done — `INSTANCE_ADMIN_EMAILS` + `/admin` |

---

## 7. Related

| Doc | Why |
| --- | --- |
| [`crates/fleet-auth/README.md`](../../../crates/fleet-auth/README.md) | Route table + source map |
| [`docs/components/ui/README.md`](../ui/README.md) | Browser routes and shell |
| [`docs/architecture/README.md`](../../architecture/README.md) | Control plane (orchestrator later) |
