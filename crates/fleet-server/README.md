# fleet-server

**Single entrypoint** for the Agent Fleet HTTP backend.

```bash
# workspace root, with .env (DATABASE_URL, JWT_SECRET, …)
cargo run
# → http://127.0.0.1:3001
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

Future domain crates (orchestrator, sandboxes, …) merge into the same process
the same way — one port, one `cargo run`.

## Env

See [`.env.example`](../../.env.example).

| Variable | Default | Notes |
| --- | --- | --- |
| `DATABASE_URL` | required | Neon / Supabase / local |
| `JWT_SECRET` | required (≥32 chars) | |
| `HOST` | `127.0.0.1` | also accepts legacy `AUTH_HOST` |
| `PORT` | `3001` | also accepts legacy `AUTH_PORT` |
| `CORS_ORIGINS` | UI localhost | comma-separated |

## Layout

```
fleet-ui  →  fleet-server  →  fleet-auth (lib)
                    ↓
                 fleet-db  →  Postgres
```
