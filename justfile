# Agent Fleet — local development
#
#   just setup   # once: create .env.local + start Postgres
#   just dev     # Postgres + API (:8080) + UI (:3030)
#
# Env is loaded from `.env.local` (see `.env.local.example`).

set shell := ["bash", "-euo", "pipefail", "-c"]
set dotenv-filename := ".env.local"
set dotenv-load := true

export RUST_LOG := env_var_or_default("RUST_LOG", "info,sqlx=warn,fleet_db=info,fleet_auth=info,fleet_server=info,fleet_orchestrator=info")

ui_port := env_var_or_default("UI_PORT", "3030")

# List recipes (default).
default:
    @just --list

# Create `.env.local` from the example if missing, then start Postgres.
setup: _env-init db-up
    @echo ""
    @echo "Ready. Run the full stack with:  just dev"
    @echo "  API  http://localhost:8080"
    @echo "  UI   http://localhost:{{ ui_port }}"

# Create `.env.local` from `.env.local.example` when it does not exist.
_env-init:
    #!/usr/bin/env bash
    if [[ -f .env.local ]]; then
      echo ".env.local already exists"
    else
      cp .env.local.example .env.local
      echo "Created .env.local from .env.local.example"
    fi

# Fail fast if local env is missing.
_require-env:
    #!/usr/bin/env bash
    if [[ ! -f .env.local ]]; then
      echo "Missing .env.local — run: just setup" >&2
      exit 1
    fi

# Start Postgres (docker compose) and wait until healthy.
db-up:
    #!/usr/bin/env bash
    docker compose up -d postgres
    echo "Waiting for Postgres…"
    for i in $(seq 1 60); do
      if docker compose exec -T postgres pg_isready -U fleet -d fleet >/dev/null 2>&1; then
        echo "Postgres is ready (localhost:5433)"
        exit 0
      fi
      sleep 1
    done
    echo "Postgres did not become ready in time" >&2
    exit 1

# Stop compose services (keeps volume).
db-down:
    docker compose down

# Stop compose services and delete the Postgres volume.
db-reset:
    docker compose down -v

# Tail Postgres logs.
db-logs:
    docker compose logs -f postgres

# Build workspace (debug).
build:
    cargo build

# Build release binaries.
build-release:
    cargo build --release

# Unit tests (no Postgres). fleet-ui is Dioxus/WASM — excluded.
# fleet-chat-md / fleet-runners-view (extracted from UI) are in --workspace --lib.
# fleet-driver ACP/worker tests live in crates/fleet-driver/tests/ (not --lib).
test:
    cargo test --workspace --lib --exclude fleet-ui --locked
    cargo test -p fleet-driver --locked
    cargo test -p fleet-host --locked

# Integration tests that need Postgres (`just db-up` + `.env.local` / DATABASE_URL).
test-integration: _require-env db-up
    cargo test -p fleet-orchestrator --locked \
      --test store_integration \
      --test tick_integration \
      --test api_integration \
      --test e2e_matrix \
      --test process_sandbox \
      --test host_pool_spawn \
      --test host_update_tick \
      --test plane_runner_tick \
      --test assignable_runner
    cargo test -p fleet-auth --locked --tests
    cargo test -p fleet-server --locked --tests
    cargo test -p fleet-db --locked --tests

# cargo check the whole workspace.
check:
    cargo check --workspace

# fleet-server: auth + workspaces + orchestrator (http://HOST:PORT from .env.local).
server: _require-env
    cargo run -p fleet-server

# Dioxus UI (http://localhost:UI_PORT, default 3030).
ui:
    dx serve --package fleet-ui --port {{ ui_port }}

# Orchestrator is a library inside fleet-server (no standalone binary).
orch: server

# Scripted orch demo (ADMIN_TOKEN + running orch/server with /v1).
demo scenario="happy":
    SCENARIO={{ scenario }} ./scripts/demo-orch.sh

# API + UI after Postgres is up (dependencies run in parallel).
[parallel]
_app: server ui

# Full local stack: Postgres, then API + UI together.
dev: _require-env db-up _app

# Launch (if needed) and walk a human through the product. See docs/human-testing.md.
human-test:
    ./scripts/human-test.sh

# Agent: record Playwright demo .webm for the GitLab MR (not the human).
human-test-record *scenes:
    ./scripts/human-test.sh record {{ scenes }}

# Format Rust sources.
fmt:
    cargo fmt --all

# Clippy the workspace.
clippy:
    cargo clippy --workspace --all-targets -- -D warnings

# Stage fleet-host / fleet-worker / fleet-driver for curl|sh.
# Args: (none|--native) host toolchain; --bookworm glibc 2.36; --aarch64 linux-aarch64; --armv7 linux-armv7; --musl; --aarch64-musl; --armv7-musl; --windows windows-x86_64; --windows-arm64; --portable bookworm+musl.
stage-worker-dist *args:
    ./scripts/stage-worker-dist.sh {{args}}
