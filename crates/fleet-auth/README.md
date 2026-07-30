# fleet-auth

**Library** — signup/login/JWT cookie handlers. Not run alone.

Mounted by [`fleet-server`](../fleet-server/):

```bash
cargo run   # starts fleet-server, which merges fleet-auth routes
```

## Routes (on fleet-server)

| Method | Path |
| --- | --- |
| `POST` | `/api/auth/signup` |
| `POST` | `/api/auth/login` |
| `POST` | `/api/auth/logout` |
| `POST` | `/api/auth/refresh` |
| `GET` | `/api/auth/me` |

DB access goes through [`fleet-db`](../fleet-db/).
