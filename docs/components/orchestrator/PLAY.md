# Play with orchestrator + UI

Local loop for the **fleet-orchestrator** control plane (mounted on **fleet-server**) and the **Fleet play** page.

## Recommended: one API process

```bash
# From repo root
cp -n .env.example .env
# .env needs:
#   DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable
#   JWT_SECRET=dev-change-me-to-a-long-random-secret-at-least-32-chars
#   ADMIN_TOKEN=dev-admin-token-change-me
#   DATA_DIR=/tmp/fleet-orch-data

docker compose up -d postgres

# Terminal 1 — unified API (auth + workspaces + /v1 orchestrator)
cargo run -p fleet-server
# → http://localhost:8080
#    GET  /healthz
#    POST /v1/projects
#    POST /api/auth/login

# Terminal 2 — UI
dx serve --package fleet-ui --port 3030
# → http://localhost:3030
```

### UI flow

1. Signup / login.
2. Sidebar → **Fleet play**.
3. Orchestrator URL: `http://localhost:8080` (same as auth).
4. Admin token: value of `ADMIN_TOKEN`.
5. Ping → Create project → Start 2-task fixture → **Run tick** a few times.

### CLI smoke

```bash
export ADMIN_TOKEN=dev-admin-token-change-me
./scripts/demo-orch.sh
```

## Optional: standalone orchestrator on :8081

```bash
HTTP_ADDR=127.0.0.1:8081 cargo run -p fleet-orchestrator
# UI: set Orchestrator URL to http://localhost:8081
```

## Notes

- Auth **workspace** id ≠ orchestrator **project** id (play mode).
- Orch migrations use version `20260728120000_*` so they co-exist with `fleet-db` migrations on one database.
- Production: set a strong `ADMIN_TOKEN`; later replace with workspace-scoped JWT auth.
