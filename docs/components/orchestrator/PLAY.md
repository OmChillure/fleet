# Play with orchestrator + UI

Local loop for the **fleet-orchestrator** control plane and the **Fleet play** page in `fleet-ui`.

## Processes (3 terminals)

```bash
# From repo root — copy env if needed
cp -n .env.example .env

# Terminal 1 — Postgres for orchestrator (host port 5433)
docker compose up -d postgres

# Terminal 2 — orchestrator API
export DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable
export ADMIN_TOKEN=dev-admin-token-change-me
export DATA_DIR=/tmp/fleet-orch-data
export HTTP_ADDR=127.0.0.1:8081
export AUTH_MODE=token
cargo run -p fleet-orchestrator
# → http://127.0.0.1:8081/healthz

# Terminal 3 — fleet-server (auth / workspaces) on :8080
# Needs its own DATABASE_URL (Neon or local agent_fleet DB) + JWT_SECRET in .env
cargo run -p fleet-server
# → http://localhost:8080

# Terminal 4 — UI
dx serve --package fleet-ui --port 3030
# → http://localhost:3030
```

## UI flow

1. Signup / login (fleet-server cookies).
2. Open **Fleet play** in the sidebar (`/{workspace_id}/fleet`).
3. Confirm orchestrator URL `http://localhost:8081` and admin token.
4. **Ping /healthz** → should show `ok`.
5. **Create project** → note project id (saved in localStorage).
6. **Start 2-task fixture**.
7. Press **Run tick** a few times until tasks are `done_success` and the run is `completed`.

## Notes

- Auth workspace and orchestrator project are **not** the same ID yet (play mode only).
- Longer-term: mount orchestrator routes under `fleet-server` and map workspace → project.
- Use `HTTP_ADDR=127.0.0.1:8081` so cookies/CORS stay simple next to UI on `:3030`.
