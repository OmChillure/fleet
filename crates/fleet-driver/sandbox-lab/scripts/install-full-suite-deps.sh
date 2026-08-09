#!/usr/bin/env bash
# Install host deps for the fleet-driver full lab suite. Does NOT run tests.
#
#   sudo ./crates/fleet-driver/sandbox-lab/scripts/install-full-suite-deps.sh
#
# After this, the invoking user can `docker` without a new login (ACL on the
# socket) and Ollama is reachable from lab containers via host.docker.internal.
set -euo pipefail

if [[ "${EUID}" -ne 0 ]]; then
  echo "re-run with sudo: sudo $0" >&2
  exit 1
fi

REAL_USER="${SUDO_USER:-}"
if [[ -z "${REAL_USER}" || "${REAL_USER}" == "root" ]]; then
  echo "run via sudo from your normal account (need SUDO_USER)" >&2
  exit 1
fi
REAL_HOME="$(getent passwd "${REAL_USER}" | cut -d: -f6)"
REAL_UID="$(id -u "${REAL_USER}")"
REAL_GID="$(id -g "${REAL_USER}")"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAB="$(cd "${SCRIPT_DIR}/.." && pwd)"
AGENTS="$(cd "${LAB}/../agents" && pwd)"
REPO="$(cd "${LAB}/../../.." && pwd)"

OLLAMA_MODEL="${OLLAMA_MODEL:-llama3.2:1b}"
echo "==> install as root for user=${REAL_USER} home=${REAL_HOME}"
echo "    repo=${REPO}"
echo "    ollama model=${OLLAMA_MODEL}"

# --- packages ---------------------------------------------------------------
need_cmd() { command -v "$1" >/dev/null 2>&1; }

if command -v dnf >/dev/null 2>&1; then
  # Fedora: engine is moby-engine; compose is often already a docker plugin.
  pkgs=()
  rpm -q moby-engine >/dev/null 2>&1 || rpm -q docker >/dev/null 2>&1 || pkgs+=(moby-engine)
  rpm -q docker-cli >/dev/null 2>&1 || pkgs+=(docker-cli)
  rpm -q git >/dev/null 2>&1 || pkgs+=(git)
  rpm -q python3 >/dev/null 2>&1 || pkgs+=(python3)
  rpm -q python3-pip >/dev/null 2>&1 || pkgs+=(python3-pip)
  rpm -q curl >/dev/null 2>&1 || pkgs+=(curl)
  rpm -q openssl >/dev/null 2>&1 || pkgs+=(openssl)
  rpm -q acl >/dev/null 2>&1 || pkgs+=(acl)
  rpm -q redis >/dev/null 2>&1 || rpm -q valkey-compat-redis >/dev/null 2>&1 || pkgs+=(valkey-compat-redis)
  if ! docker compose version >/dev/null 2>&1; then
    pkgs+=(docker-compose docker-compose-plugin)
  fi
  if [[ ${#pkgs[@]} -gt 0 ]]; then
    echo "==> dnf install: ${pkgs[*]}"
    dnf install -y --skip-unavailable "${pkgs[@]}"
  else
    echo "==> dnf: required rpms already installed"
  fi
elif command -v apt-get >/dev/null 2>&1; then
  apt-get update -y
  DEBIAN_FRONTEND=noninteractive apt-get install -y \
    docker.io docker-compose-v2 \
    git python3 python3-pip curl openssl \
    redis-server \
    acl
else
  echo "unsupported distro (need dnf or apt-get)" >&2
  exit 1
fi

if ! need_cmd docker; then
  echo "docker CLI missing after install" >&2
  exit 1
fi
if ! docker compose version >/dev/null 2>&1; then
  echo "docker compose plugin missing. install docker-compose and re-run." >&2
  exit 1
fi

systemctl enable --now docker

# docker group + socket ACL so THIS login can docker without re-login
getent group docker >/dev/null || groupadd docker
usermod -aG docker "${REAL_USER}"
if command -v setfacl >/dev/null 2>&1; then
  setfacl -m "u:${REAL_USER}:rw" /var/run/docker.sock || true
fi
chmod 666 /var/run/docker.sock || true

docker version >/dev/null
docker compose version >/dev/null
echo "==> docker ok: $(docker version --format '{{.Server.Version}}')"

# --- ollama: listen on all interfaces so compose extra_hosts can reach it ---
mkdir -p /etc/systemd/system/ollama.service.d
cat >/etc/systemd/system/ollama.service.d/fleet-lab.conf <<'EOF'
[Service]
Environment="OLLAMA_HOST=0.0.0.0:11434"
EOF
if systemctl list-unit-files | grep -q '^ollama.service'; then
  systemctl daemon-reload
  systemctl enable --now ollama
  systemctl restart ollama
else
  echo "==> no ollama.service; starting ollama serve as ${REAL_USER}"
  if ! pgrep -x ollama >/dev/null; then
    sudo -u "${REAL_USER}" env OLLAMA_HOST=0.0.0.0:11434 \
      nohup ollama serve >/tmp/ollama-fleet-lab.log 2>&1 &
    sleep 2
  fi
fi

for _ in $(seq 1 30); do
  if curl -sf http://127.0.0.1:11434/api/tags >/dev/null; then
    break
  fi
  sleep 0.5
done
curl -sf http://127.0.0.1:11434/api/tags >/dev/null

echo "==> pulling ollama model ${OLLAMA_MODEL} (as ${REAL_USER})"
sudo -u "${REAL_USER}" ollama pull "${OLLAMA_MODEL}"

# OpenAI-compatible probe
if ! curl -sf http://127.0.0.1:11434/v1/models | head -c 80; then
  echo
  echo "warn: /v1/models failed; /api/tags:" >&2
  curl -s http://127.0.0.1:11434/api/tags || true
fi
echo

# --- dummy SSH key (compose always bind-mounts this path) -------------------
SSH_DIR=/tmp/fleet-lab-ssh
mkdir -p "${SSH_DIR}"
if [[ ! -f "${SSH_DIR}/id_ed25519" ]]; then
  ssh-keygen -t ed25519 -f "${SSH_DIR}/id_ed25519" -N "" -C "fleet-lab-dummy"
fi
chown -R "${REAL_UID}:${REAL_GID}" "${SSH_DIR}"
chmod 700 "${SSH_DIR}"
chmod 600 "${SSH_DIR}/id_ed25519"

# --- lab .env (ollama, not LM Studio LAN IP) --------------------------------
ENV_FILE="${LAB}/.env"
cat >"${ENV_FILE}" <<EOF
REDIS_URL=redis://redis:6379/0
REDIS_URL_HOST=redis://127.0.0.1:6379/0
FLEET_JOBS_QUEUE=fleet:jobs
FLEET_RESULTS_QUEUE=fleet:results
HARNESS=opencode
GIT_REPO_URL=https://github.com/eugenepyvovarov/123-invoices.git
GIT_REF=main
SSH_KEY_HOST_PATH=${SSH_DIR}/id_ed25519
OPENCODE_PROVIDER_ID=ollama
OPENCODE_PROVIDER_NAME=Ollama (local)
OPENCODE_PROVIDER_NPM=@ai-sdk/openai-compatible
OPENCODE_BASE_URL=http://host.docker.internal:11434/v1
OPENCODE_API_KEY=ollama
MODEL=ollama/${OLLAMA_MODEL}
JOB_TIMEOUT_SEC=1200
AUTO_SEED_JOB=0
FLEET_DRIVER_LOOP=0
FLEET_PROVISION_MODE=volume
FLEET_DIR=/workspace/.fleet
FLEET_HANDSHAKE_SECRET=dev-lab-handshake-not-for-prod
EOF
chown "${REAL_UID}:${REAL_GID}" "${ENV_FILE}"
echo "==> wrote ${ENV_FILE}"

# --- images -----------------------------------------------------------------
echo "==> pulling redis + workspace base"
docker pull redis:7-alpine
docker pull python:3.13-bookworm
docker pull rust:1.88-bookworm

echo "==> building harness images (base + opencode + pi)"
(
  cd "${AGENTS}"
  docker build -t agent-fleet-base:alpine -f base/Dockerfile base
  docker build -t agent-fleet-opencode:alpine \
    --build-arg BASE_IMAGE=agent-fleet-base:alpine \
    -f opencode/Dockerfile opencode
  docker build -t agent-fleet-pi:alpine \
    --build-arg BASE_IMAGE=agent-fleet-base:alpine \
    -f pi/Dockerfile pi
  if [[ "${ALL_HARNESSES:-0}" == "1" ]]; then
    bash "${AGENTS}/build.sh"
  fi
)

echo "==> building lab workspace + driver (opencode base)"
cd "${LAB}"
export DRIVER_BASE_IMAGE=agent-fleet-opencode:alpine
docker compose build workspace
docker compose build --pull=false driver
docker tag "$(basename "${LAB}")-workspace" agent-fleet-lab-workspace:local 2>/dev/null || true
docker tag "$(basename "${LAB}")-driver" agent-fleet-lab-driver:local 2>/dev/null || true

echo
echo "==> install complete for ${REAL_USER}"
echo "    docker (no sudo, socket ACL + group docker)"
echo "    ollama ${OLLAMA_MODEL} on 0.0.0.0:11434"
echo "    ${ENV_FILE}"
echo "    ssh dummy key ${SSH_DIR}/id_ed25519"
echo "    images: agent-fleet-{base,opencode,pi}:alpine + compose workspace/driver"
echo
echo "Do not run tests in this script. As ${REAL_USER}, the agent will run the suite."
