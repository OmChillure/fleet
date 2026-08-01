# Play with Fleet (proper workspace hookup)

## Mapping

`workspace_id` **is** the orchestrator `project_id`. Opening **Fleet** ensures that project exists for your logged-in workspace.

## Run

```bash
docker compose up -d postgres

# .env (minimum)
# DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable
# JWT_SECRET=…(≥32 chars)
# DATA_DIR=/tmp/fleet-orch-data
# ADMIN_TOKEN is optional (only for /v1 scripts)

cargo run -p fleet-server          # :8080
dx serve --package fleet-ui --port 3030
```

1. Signup / login  
2. Sidebar → **Fleet**  
3. **Start 2-task run** → **Run tick** until board columns show Done  

Cookie auth on `/api/workspaces/{id}/fleet/*`. **No ADMIN_TOKEN required.**

Optional: set `ADMIN_TOKEN` to enable `/v1/*` bearer automation (`./scripts/demo-orch.sh`).
