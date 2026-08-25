# Development guide — run Agent Fleet

This guide gets **Postgres + API + UI** running together.

| Service | URL | Compose service |
|---------|-----|-----------------|
| Postgres | `localhost:5433` | `postgres` |
| API (`fleet-server`) | http://localhost:8080 | `api` |
| UI (`fleet-ui`) | http://localhost:3030 | `ui` (nginx + WASM; `/api` proxied) |

Prefer **`http://localhost:3030`** in the browser (not `127.0.0.1`) so cookie auth matches CORS/proxy setup.

---

## Prerequisites

- **Docker** + Docker Compose v2
- For **k3d**: `brew install k3d` (see [`infra/README.md`](infra/README.md))
- Host toolchain (`cargo`, `dx`, kind, …): one command, no flags —

  `crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh`

  Persist `PROFILE` / `ENABLE_*` in repo-root `.env`. Do not `cargo install`
  dioxus-cli on the host (OpenSSL 4 cannot build it; the install script
  builds `dx` inside `rust:<workspace>-bookworm`).

---

## One command: full stack in Docker

```bash
# From repo root
docker compose up -d --build
docker compose ps
```

| Check | Command |
|-------|---------|
| API health | `curl -s http://localhost:8080/health` |
| UI | open http://localhost:3030 |
| UI → API proxy | `curl -s http://localhost:3030/health` |

Migrations run on API startup (auth, integrations, personas, orchestrator).

### Kubernetes (k3d)

Same host URLs, one small k3s cluster:

```bash
brew install k3d
make -C infra k3d-create
make -C infra k3d-load      # build + import api/ui/operator images
make -C infra apply-k3d
make -C infra k3d-delete    # tear down
```

See [`infra/README.md`](infra/README.md).

### First-time use

1. Open http://localhost:3030  
2. **Sign up** → login → workspace dashboard  
3. **Agents** → **Seed starters** for personas  
4. **Workers** — add a worker and copy the install/pair command (token panel is enough; no second machine). [`docs/components/worker-host/README.md`](docs/components/worker-host/README.md)

### Tear down

```bash
docker compose down          # keep DB volume
docker compose down -v       # wipe Postgres + API data volumes
```

### Rebuild a single service

```bash
docker compose up -d --build api
docker compose up -d --build ui
```

---

## How the UI container works

- **`Dockerfile.ui`** builds the Dioxus web app (`dx build --release --platform web`)
- **nginx** serves static WASM/HTML on port **3030**
- **`deploy/nginx-ui.conf`** proxies `/api/*` → `http://api:8080/api/`  
  so the browser uses **same-origin** `/api` (cookies stay first-party, same as local `dx serve`)

Images:

| Dockerfile | Image | Role |
|------------|-------|------|
| `Dockerfile` | `agent-fleet-api:local` | fleet-server + fleet-host + fleet-driver |
| `Dockerfile.ui` | `agent-fleet-ui:local` | nginx + static UI |

---

## Host development (fast Rust / UI iteration)

### Option A — Compose DB + API only, UI on host

```bash
docker compose up -d postgres api
dx serve --package fleet-ui --port 3030
```

`Dioxus.toml` proxies `/api` → `http://127.0.0.1:8080/api/`.

### Option B — Everything on host

```bash
docker compose up -d postgres
cp -n .env.example .env
# DATABASE_URL=postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable
export DATABASE_URL=postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable
export JWT_SECRET=dev-change-me-to-a-long-random-secret-at-least-32-chars
export DATA_DIR=./data
cargo run -p fleet-server
# other terminal
dx serve --package fleet-ui --port 3030
```

### Option C — `fleet-dev.sh` (nohup)

```bash
./scripts/fleet-dev.sh setup
./scripts/fleet-dev.sh start
./scripts/fleet-dev.sh status
./scripts/fleet-dev.sh logs
./scripts/fleet-dev.sh stop
```

---

## Environment (API)

Compose sets these for the `api` service. Host runs can use `.env`:

```env
DATABASE_URL=postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable
JWT_SECRET=dev-change-me-to-a-long-random-secret-at-least-32-chars
HOST=127.0.0.1
PORT=8080
CORS_ORIGINS=http://localhost:3030,http://127.0.0.1:3030
COOKIE_SECURE=false
API_PUBLIC_BASE=http://localhost:8080
UI_PUBLIC_BASE=http://localhost:3030
DATA_DIR=./data
SANDBOX_BACKEND=stub
```

| Variable | Notes |
|----------|--------|
| `DATABASE_URL` | Host → port **5433**; inside Compose network → `postgres:5432` |
| `CORS_ORIGINS` | Needed if the browser calls the API origin directly |
| `API_PUBLIC_BASE` | OAuth callbacks, absolute API links |
| `UI_PUBLIC_BASE` | OAuth return / UI links |
| `SANDBOX_BACKEND` | `stub` (default), `process`, `docker`, or `k8s` |
| `PROCESS_WORKER_BIN` | e.g. `/usr/local/bin/fleet-driver` in the API image |
| `K8S_*` | Only when `SANDBOX_BACKEND=k8s`. See [`docs/components/orchestrator/PLAY.md`](docs/components/orchestrator/PLAY.md). Worker callback must not be loopback. |
| `ADMIN_TOKEN` | Optional bearer for `/v1` automation scripts |

---

## Personas

- Design: [`docs/components/personas/plan.html`](docs/components/personas/plan.html)
- UI: **Agents** in the sidebar  
- API: `/api/workspaces/{id}/personas` (+ `seed-starters`)

No LLM is called yet; personas store role prompts and model preferences for a future driver.

---

## Process sandbox + real worker (optional)

```bash
# Host
cargo build -p fleet-driver --bins
export SANDBOX_BACKEND=process
export PROCESS_WORKER_BIN="$PWD/target/debug/fleet-driver"
export FLEET_HARNESS=mock
export API_PUBLIC_BASE=http://127.0.0.1:8080
cargo run -p fleet-server
```

Then in the UI: **Fleet → Start 2-task run → Run until done**. Sessions appear on Fleet and **Sandboxes**. Artifacts land under `$DATA_DIR/projects/<workspace_id>/sessions/<session_id>/artifacts/`.

In Compose, set on `api`:

```yaml
SANDBOX_BACKEND: process
PROCESS_WORKER_BIN: /usr/local/bin/fleet-driver
```

### Kubernetes sandboxes

In-cluster API is `SANDBOX_BACKEND=stub` (not a runner). `fleet-host`
creates `FleetSandbox` CRs; the operator must be running
([`infra/README.md`](infra/README.md)). Host `.env` is unchanged. Optional
host API against a cluster is no longer the spawn path — pair `fleet-host`
instead.

```bash
export SANDBOX_BACKEND=k8s
export K8S_WORKER_API_URL=http://api.agent-fleet.svc.cluster.local:8080
# Host API + cluster: a host IP pods can route — not 127.0.0.1, not DNS
# export K8S_WORKER_API_URL=http://172.18.0.1:8080
cargo run -p fleet-server
```

---

## Useful endpoints

| Path | Purpose |
|------|---------|
| `GET /health` | API + DB |
| `GET /api` | Route index |
| `POST /api/auth/signup` | User + default workspace |
| `GET /api/workspaces/{id}/personas` | Personas |
| `GET /api/workspaces/{id}/integrations` | OAuth catalog |
| UI `/{workspace_id}/agents` | Personas UI |
| UI `/{workspace_id}/fleet` | Fleet play |

---

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| UI blank / 502 on `/api` | `docker compose ps` — wait until `api` is healthy; `docker compose logs api` |
| Login cookies fail | Use **localhost** for UI; avoid mixing `127.0.0.1` and `localhost` |
| `DATABASE_URL` errors on host | Host port is **5433**, not 5432 |
| UI image build is slow | First `dx` + WASM release build is long; later layers cache |
| Stale UI after code change | `docker compose up -d --build ui` |
| Port 3030/8080 in use | Stop host `dx serve` / old containers |
| Personas empty | Seed starters on Agents page; confirm correct workspace |

---

## Repo map (short)

| Path | Role |
|------|------|
| `docker-compose.yml` | postgres + api + **ui** + zenohd + fleet-host |
| `Dockerfile` | API image |
| `Dockerfile.ui` | UI image (dx build + nginx) |
| `Dockerfile.operator` | Kubernetes sandbox operator |
| `infra/` | Kustomize control plane + FleetSandbox CRD/operator (`make -C infra k3d-create`) |
| `deploy/nginx-ui.conf` | SPA + `/api` proxy |
| `crates/fleet-server` | Unified HTTP API |
| `crates/fleet-auth` | Auth, workspaces, personas |
| `crates/fleet-ui` | Dioxus web UI |
| `scripts/fleet-dev.sh` | Host nohup helper |
| `docs/components/personas/` | Personas design |

