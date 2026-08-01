# Play with Fleet (proper workspace hookup)

## Mapping

`workspace_id` **is** the orchestrator `project_id`. Opening **Fleet** ensures that project exists for your logged-in workspace.

## Run

```bash
docker compose up -d postgres

# .env
# DATABASE_URL=postgres://fleet:fleet@localhost:5433/fleet?sslmode=disable
# JWT_SECRET=…(≥32 chars)
# ADMIN_TOKEN=dev-admin-token-change-me   # still used for /v1 admin/automation
# DATA_DIR=/tmp/fleet-orch-data

cargo run -p fleet-server          # :8080
dx serve --package fleet-ui --port 3030
```

1. Signup / login  
2. Sidebar → **Fleet**  
3. **Start 2-task run** → **Run tick** until board columns show Done  

Cookie auth on `/api/workspaces/{id}/fleet/*`. No admin token in the UI.

Admin bearer `/v1/*` remains for scripts (`./scripts/demo-orch.sh`).
