# Agent Fleet — Alpine agent images

Small **Alpine 3.21** images for ACP harnesses used by fleet-driver (sandbox lab).

| Image | Tag | Entry (stdio ACP) | Package / install |
|-------|-----|-------------------|-------------------|
| `agent-fleet-base` | `:alpine` | `bash` | node, npm, git, curl, **gcompat** |
| `agent-fleet-codex` | `:alpine` | `codex-acp` | `@agentclientprotocol/codex-acp@1.1.7` |
| `agent-fleet-opencode` | `:alpine` | `opencode acp` | `opencode-ai@1.18.9` (musl optional deps) |
| `agent-fleet-grok` | `:alpine` | `grok agent stdio` | `https://x.ai/cli/install.sh` |
| `agent-fleet-pi` | `:alpine` | `pi-acp` | `@earendil-works/pi-coding-agent` + `pi-acp` |

## Why Alpine

- Small base, fast pull.
- OpenCode ships **musl** optional packages (`opencode-linux-*-musl`).
- Codex (`@openai/codex-*`) and official **Grok** CLI are typically **glibc** binaries — base installs Alpine **`gcompat`** so they can still run. If Grok fails at runtime with loader errors, use a Debian bookworm image for that harness only.

## Build

Requires Docker (OrbStack / Docker Desktop / engine). From **repo root**:

```bash
# all images (from repo root)
./crates/fleet-driver/agents/build.sh

# or from this directory
cd crates/fleet-driver/agents && ./build.sh

# or stepwise (cwd = crates/fleet-driver/agents)
docker build -t agent-fleet-base:alpine     -f base/Dockerfile     base
docker build -t agent-fleet-codex:alpine    -f codex/Dockerfile    codex
docker build -t agent-fleet-opencode:alpine -f opencode/Dockerfile opencode
docker build -t agent-fleet-grok:alpine     -f grok/Dockerfile     grok
docker build -t agent-fleet-pi:alpine       -f pi/Dockerfile       pi
```

Pin versions with build-args:

```bash
# cwd = crates/fleet-driver/agents
docker build -t agent-fleet-codex:alpine \
  --build-arg CODEX_ACP_VERSION=1.1.7 \
  -f codex/Dockerfile codex

docker build -t agent-fleet-opencode:alpine \
  --build-arg OPENCODE_VERSION=1.18.9 \
  -f opencode/Dockerfile opencode

docker build -t agent-fleet-grok:alpine \
  --build-arg GROK_VERSION=0.1.42 \
  -f grok/Dockerfile grok
```

Override the base tag if you rename it:

```bash
docker build --build-arg BASE_IMAGE=my-base:dev \
  -t agent-fleet-codex:alpine -f codex/Dockerfile codex
```

## Run from the host over ACP (stdio)

ACP is **JSON-RPC on stdin/stdout**. The fleet driver (or any client) stays **outside** the container and spawns Docker with **`-i` (keep stdin open)** and **no `-t`** (TTY would mangle JSON lines).

```
┌──────────────┐  stdin/stdout ACP   ┌─────────────────────────┐
│ agent-fleet  │ ──────────────────► │ docker run --rm -i …    │
│ driver (host)│ ◄────────────────── │   opencode acp (ctr)    │
└──────────────┘                     └─────────────────────────┘
```

Verified: host client → `docker run -i agent-fleet-opencode:alpine` → `initialize` + `session/new` succeeds.

### Fleet profile shape

Point the harness profile at Docker instead of a local binary:

| Field | Example |
|-------|---------|
| command | `docker` |
| args | `run --rm -i -v /abs/path/to/project:/workspace -w /workspace agent-fleet-opencode:alpine` |

Notes:

- Mount the **host project path** at `/workspace` (container `cwd` for the agent).
- Pass secrets as `-e OPENAI_API_KEY=…` / `-e XAI_API_KEY=…`, or mount auth dirs (see below). OpenCode advertises ACP `authMethods: opencode-login` but real provider keys usually come from env or a mounted `~/.local/share/opencode` config.
- Do **not** put `-m` before OpenCode’s `acp` subcommand; set model via ACP after `session/new`.

### Manual smoke

```bash
# Codex
docker run --rm -i \
  -e OPENAI_API_KEY \
  -v "$PWD:/workspace" \
  agent-fleet-codex:alpine

# OpenCode (ACP stdio — same channel the driver uses)
docker run --rm -i \
  -v "$PWD:/workspace" \
  -v "$HOME/.local/share/opencode:/home/fleet/.local/share/opencode" \
  agent-fleet-opencode:alpine

# Grok
docker run --rm -i \
  -e GROK_DEPLOYMENT_KEY \
  -e XAI_API_KEY \
  -v "$PWD:/workspace" \
  -v "$HOME/.grok/auth.json:/home/fleet/.grok/auth.json:ro" \
  agent-fleet-grok:alpine
```

Version checks (no ACP):

```bash
docker run --rm --entrypoint codex-acp agent-fleet-codex:alpine --version
docker run --rm --entrypoint opencode  agent-fleet-opencode:alpine --version
docker run --rm --entrypoint grok      agent-fleet-grok:alpine --version
```

## Layout

```
crates/fleet-driver/agents/
  README.md
  build.sh
  base/Dockerfile
  codex/Dockerfile
  opencode/Dockerfile
  grok/Dockerfile
```

## Notes for the fleet driver

Default host profiles still call local binaries (`opencode acp`, `codex-acp` / `npx …`, `grok agent stdio`). To run harnesses **inside** these images later, set the profile command to `docker` (or a thin wrapper) with `run --rm -i …` and the image above; keep `cwd` mounted at `/workspace`.

| Harness | Model selection |
|---------|-----------------|
| OpenCode | ACP `session/set_config_option` only — never `-m` before `acp` |
| Codex | CLI `--model` and/or ACP config options |
| Grok | `grok agent -m MODEL stdio` and/or `models.availableModels` |

## Troubleshooting

| Symptom | Likely cause | Fix |
|---------|--------------|-----|
| `not found` / loader error on `grok` or `codex` | glibc binary on musl without gcompat | rebuild base (includes `gcompat`); or switch grok/codex to Debian |
| `opencode -m X acp` exits / prints help | model flag before subcommand | use `opencode acp` only; set model over ACP |
| npm optional dep missing binary | wrong arch | build on `linux/arm64` or `linux/amd64` matching deploy host (`docker build --platform`) |
