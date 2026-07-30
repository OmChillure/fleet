# fleet-server

**Single entrypoint** for the Agent Fleet HTTP backend.

```bash
# workspace root, with .env (DATABASE_URL, JWT_SECRET, …)
cargo run
# → http://localhost:8080
```

Same as:

```bash
cargo run -p fleet-server
```

## What it mounts

| Path | Source |
| --- | --- |
| `GET /health` | server |
| `GET /api` | API index |
| `/api/auth/*` | [`fleet-auth`](../fleet-auth/) |
| `/api/workspaces` | [`fleet-auth`](../fleet-auth/) |

Future domain crates (orchestrator, sandboxes, …) merge into the same process
the same way — one port, one `cargo run`.

## Env

See [`.env.example`](../../.env.example).

| Variable | Default | Notes |
| --- | --- | --- |
| `DATABASE_URL` | required | Neon / Supabase / local |
| `JWT_SECRET` | required (≥32 chars) | |
| `HOST` | `localhost` | also accepts legacy `AUTH_HOST` |
| `PORT` | `8080` | also accepts legacy `AUTH_PORT` |
| `CORS_ORIGINS` | UI on `:3030` | comma-separated |

## Layout

```
fleet-ui  →  fleet-server  →  fleet-auth (lib)
                    ↓
                 fleet-db  →  Postgres
```
