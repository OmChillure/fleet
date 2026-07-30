# fleet-auth

**Library** — signup/login/JWT cookies, session validation, and workspace
list/create. Not run alone.

Mounted by [`fleet-server`](../fleet-server/):

```bash
# from workspace root
cargo run   # → http://localhost:8080  (merges fleet-auth routes)
```

DB access goes through [`fleet-db`](../fleet-db/).

---

## Routes (on fleet-server)

### Auth

| Method | Path | Auth | Description |
| --- | --- | --- | --- |
| `POST` | `/api/auth/signup` | no | Create user + default workspace; **no** session cookies |
| `POST` | `/api/auth/login` | no | Verify password; set cookies; ensure default workspace |
| `POST` | `/api/auth/logout` | cookie | Revoke refresh session; clear cookies |
| `POST` | `/api/auth/refresh` | refresh cookie | Rotate refresh + issue new access JWT |
| `GET` | `/api/auth/me` | access cookie | Current user + workspaces (ensures default) |

### Workspaces

| Method | Path | Auth | Description |
| --- | --- | --- | --- |
| `GET` | `/api/workspaces` | access cookie | List memberships; ensures a default if none |
| `POST` | `/api/workspaces` | access cookie | Create workspace; caller is `owner` |

---

## Cookies

| Cookie | Content | Notes |
| --- | --- | --- |
| `fleet_access` | JWT access token | Short TTL (`JWT_ACCESS_TTL_SECS`, default 900) |
| `fleet_refresh` | Opaque refresh token | Longer TTL; stored as SHA-256 hash in Postgres |

- `HttpOnly`, `Path=/`
- `Secure` / `SameSite` from env (`COOKIE_SECURE`, `COOKIE_SAMESITE`)
- Local default: `COOKIE_SECURE=false`, `COOKIE_SAMESITE=Lax`

Browser clients **must** send credentials (`fetch` / reqwest
`fetch_credentials_include`) and the API must allow the UI origin in
`CORS_ORIGINS` with credentials enabled.

---

## Default workspace

On **signup**, login, refresh, `GET /api/auth/me`, and `GET /api/workspaces`,
the server ensures the user has at least one workspace:

- **Name:** `{display_name}'s workspace` if set, else `{email-local}'s workspace`
- **Slug:** derived from email + short user-id suffix (unique)
- **Role:** `owner`

Login / refresh responses include:

```json
{
  "user": { "id": "…", "email": "…", "display_name": null },
  "message": "logged in",
  "default_workspace": {
    "id": "…",
    "slug": "…",
    "name": "you's workspace",
    "role": "owner"
  }
}
```

The UI uses `default_workspace.id` to open `/{workspace_id}/dashboard`.

### Create workspace body

```json
{ "name": "Acme Eng", "slug": "optional-slug" }
```

`slug` is optional; if omitted it is slugified from `name` plus a short id
suffix.

---

## Source layout

```text
src/
  config.rs          # JWT + cookie settings from env
  cookies.rs         # Set-Cookie helpers
  tokens.rs          # JWT issue/verify, refresh hashing
  password.rs        # argon2
  models.rs          # HTTP DTOs
  routes/
    auth.rs          # signup / login / logout / refresh / me
    workspaces.rs    # list / create + ensure_default_workspace
  state.rs           # AuthState (pool + config)
```

---

## Related

- Server mount + env: [`fleet-server/README.md`](../fleet-server/README.md)
- Schema / repos: [`fleet-db/README.md`](../fleet-db/README.md)
- Design notes: [`docs/components/auth/README.md`](../../docs/components/auth/README.md)
- UI client: [`fleet-ui` `pages/auth/api.rs`](../fleet-ui/src/pages/auth/api.rs)
