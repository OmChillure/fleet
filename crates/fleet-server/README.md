# fleet-server

**Single entrypoint** for the Agent Fleet HTTP backend.

```bash
# workspace root, with .env (DATABASE_URL, JWT_SECRET, …)
cargo run
# → http://localhost:8080   (default HOST=127.0.0.1 PORT=8080)
```

Same as:

```bash
cargo run -p fleet-server
```

Pair with the UI:

```bash
dx serve --package fleet-ui --port 3030
# → http://localhost:3030
```

---

## What it mounts

| Path | Source |
| --- | --- |
| `GET /health` | server — process + DB acquire probe |
| `GET /api` | server — API index JSON |
| `/api/auth/*` | [`fleet-auth`](../fleet-auth/) |
| `GET`/`POST` `/api/workspaces` | [`fleet-auth`](../fleet-auth/) |

Future domain crates (orchestrator, sandboxes, …) merge into the same process
the same way — one port, one `cargo run`.

---

## Env

See [`.env.example`](../../.env.example).

| Variable | Default | Notes |
| --- | --- | --- |
| `DATABASE_URL` | **required** | Neon / Supabase / local. Neon needs TLS (`?sslmode=require`) and sqlx `tls-rustls-ring` (workspace default) |
| `JWT_SECRET` | **required** (≥32 chars) | Signs access JWTs |
| `JWT_ACCESS_TTL_SECS` | `900` | Access cookie TTL |
| `JWT_REFRESH_TTL_SECS` | `604800` | Refresh cookie / session TTL |
| `HOST` | `127.0.0.1` | Bind address (use an IP; `localhost` is not a valid `SocketAddr` host) |
| `PORT` | `8080` | Bind port (also accepts legacy `AUTH_PORT`) |
| `CORS_ORIGINS` | `http://localhost:3030,http://127.0.0.1:3030` | Comma-separated UI origins; credentials enabled |
| `COOKIE_SECURE` | `false` | Set `true` behind HTTPS |
| `COOKIE_SAMESITE` | `Lax` | `Strict` / `Lax` / `None` (`None` needs Secure) |

---

## Layout

```text
fleet-ui (:3030)
    │  cookie credentials + CORS
    ▼
fleet-server (:8080)
    ├── fleet-auth  (lib routes)
    └── fleet-db    → Postgres
```

---

## Related

- Auth + workspace API details: [`fleet-auth/README.md`](../fleet-auth/README.md)
- DB layer: [`fleet-db/README.md`](../fleet-db/README.md)
- Component doc: [`docs/components/auth/README.md`](../../docs/components/auth/README.md)
