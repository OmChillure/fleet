#!/usr/bin/env bash
# =============================================================================
# Agent Fleet — host install (ONLY source of truth for install)
# =============================================================================
# Path: crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh
# Does NOT run tests. Re-run is safe (skips existing image tags unless FORCE_REBUILD=1).
#
# An agent reading this file should:
#   1) Detect OS / RAM / disk (hints below).
#   2) Persist PROFILE/ENABLE_* in the repo-root .env (same file as the rest of
#      the project). Never pass them on the command line.
#   3) Tell the human to run this script with zero arguments (Linux self-sudos).
#   4) Not invent a second install doc — this file is the doc.
#
# Command (same everywhere — no flags, no PROFILE= prefix):
#   /abs/path/to/crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh
# Linux elevates itself with sudo. macOS/Git Bash run as the current user
# (Homebrew must not be root; script uses SUDO_USER / $USER for brew).
# Windows: WSL2 Ubuntu (preferred) or Git Bash + Docker Desktop. Not cmd/PS.
#
# -----------------------------------------------------------------------------
# Capacity — agent evaluation
# -----------------------------------------------------------------------------
# Measure before choosing a profile:
#   Linux:  free -g; df -h .
#   macOS:  sysctl -n hw.memsize; df -h .
#   WSL:    same as Linux inside the distro (Docker Desktop RAM is a separate cap).
#
# PROFILE=full     ~16 GiB RAM, ~25–40 GiB disk. kind + api/ui/operator rust
#                  images + all harnesses + ollama + lab. Default on big hosts.
# PROFILE=lab      ~8 GiB RAM, ~8–15 GiB disk. Sandbox compose e2e only (no kind,
#                  no api/ui/operator images). Enough for fleet-sandbox-driver work.
# PROFILE=minimal  ~4–8 GiB RAM, ~4–8 GiB disk. OpenCode harness only, no
#                  grok/codex, no kind, no git-ssh, no full-stack images.
#
# ENABLE_* (0/1). Unset → take PROFILE default. Explicit 0/1 always wins.
#   ENABLE_SANDBOX_LAB         compose images + host Postgres :5433 + sandbox .env
#   ENABLE_LAB                 alias for ENABLE_SANDBOX_LAB (back-compat)
#   ENABLE_HARNESS_OPENCODE    agent-fleet-opencode:alpine (+ base)
#   ENABLE_HARNESS_PI          agent-fleet-pi:alpine
#   ENABLE_HARNESS_CODEX       agent-fleet-codex:alpine
#   ENABLE_HARNESS_GROK        agent-fleet-grok:alpine
#   ENABLE_FULL_IMAGES         agent-fleet-{api,ui,operator}:local (multi-GB rust)
#   ENABLE_KIND                kind cluster + kubectl apply -k infra/overlays/kind
#                              (requires ENABLE_FULL_IMAGES=1)
#   ENABLE_OLLAMA              install/bind Ollama + pull OLLAMA_MODEL
#   ENABLE_LMSTUDIO_PROXY      TCP :1234 → :11434 so LM Studio job URLs work
#   ENABLE_GITSSH              local git+ssh container :2222 (tiny)
#   ENABLE_BROWSER_E2E         pip playwright + Chromium under <repo>/.tmp/ms-playwright
#                              + Linux OS libs (`playwright install-deps chromium`).
#                              Headless UI e2e in run-full-suite.sh. Default on
#                              only for PROFILE=full (kind publishes :3030/:8080).
# FORCE_REBUILD=1              rebuild docker tags even if they exist
# OLLAMA_MODEL                 default llama3.2:1b (small). Do not pull 70B on
#                              minimal hosts.
#
# Back-compat aliases (still honored):
#   SKIP_KIND=1            → ENABLE_KIND=0
#   SKIP_FULL_IMAGES=1     → ENABLE_FULL_IMAGES=0
#   SKIP_HARNESS=1         → all ENABLE_HARNESS_*=0
#
# -----------------------------------------------------------------------------
# OS matrix (logic below; keep this table in sync)
# -----------------------------------------------------------------------------
# Linux + systemd     packages via dnf|apt|pacman|zypper|apk; docker.sock ACL;
#                     ollama.service drop-in OLLAMA_HOST=0.0.0.0:11434
# Linux, no systemd   same packages; ollama/socat via setsid (not a child of this
#                     script — otherwise bash never exits)
# macOS               Homebrew as the login user (never brew-as-root). Docker
#                     Desktop or Colima must already run. launchd/brew services
#                     or setsid for ollama + :1234 proxy. No docker.sock chmod.
# Windows + WSL2      treat as Linux (install docker-ce or use Docker Desktop
#                     WSL integration).
# Windows + Git Bash  no pacman/apt. Require docker, git, python3, curl, ssh
#                     already on PATH (Docker Desktop). Skip systemd, chown if
#                     it fails, usermod, setfacl. ENABLE_KIND default 0.
# Windows cmd/PS      not supported — tell the user to open WSL2 or Git Bash.
#
# Rust: workspace rust-version (Cargo.toml) must match Dockerfile FROM rust:<ver>.
# Fail fast if any locked crate's rust-version is newer than that toolchain.
# Do not hard-code crate names/versions in this script.
# Scratch files: <repo>/.tmp/ (gitignored) — never sticky system /tmp.
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_PATH="${SCRIPT_DIR}/$(basename "${BASH_SOURCE[0]}")"
LAB="$(cd "${SCRIPT_DIR}/.." && pwd)"
AGENTS="$(cd "${LAB}/../agents" && pwd)"
REPO="$(cd "${LAB}/../../.." && pwd)"
ROOT_ENV="${REPO}/.env"

if [[ $# -gt 0 ]]; then
  echo "ERROR: this script takes no arguments." >&2
  echo "       Persist PROFILE/ENABLE_* in ${ROOT_ENV}" >&2
  echo "       Run: ${SCRIPT_PATH}" >&2
  exit 2
fi

# Repo-root .env (gitignored) — same file the server/UI/orchestrator use.
if [[ -f "${ROOT_ENV}" ]]; then
  echo "==> loading ${ROOT_ENV}"
  set -a
  # shellcheck disable=SC1090
  . "${ROOT_ENV}"
  set +a
fi

# --- identity / OS ----------------------------------------------------------
UNAME_S="$(uname -s 2>/dev/null || echo unknown)"
case "${UNAME_S}" in
  Linux*)   OS_FAMILY=linux ;;
  Darwin*)  OS_FAMILY=darwin ;;
  MINGW*|MSYS*|CYGWIN*|Windows_NT) OS_FAMILY=windows ;;
  *)
    echo "ERROR: unsupported kernel '${UNAME_S}'. Use Linux, macOS, WSL2, or Git Bash." >&2
    exit 1
    ;;
esac

# Linux needs root for packages/systemd. Re-exec so the human command never includes sudo.
if [[ "${OS_FAMILY}" == linux && "$(id -u)" -ne 0 ]]; then
  if ! command -v sudo >/dev/null 2>&1; then
    echo "ERROR: need root (or sudo) on Linux to install packages." >&2
    exit 1
  fi
  exec sudo "${SCRIPT_PATH}"
fi

if [[ "${OS_FAMILY}" == linux ]]; then
  REAL_USER="${SUDO_USER:-}"
  if [[ -z "${REAL_USER}" || "${REAL_USER}" == "root" ]]; then
    echo "run via sudo from your normal account (need SUDO_USER)" >&2
    exit 1
  fi
else
  # macOS / Git Bash: prefer the login user even under sudo
  REAL_USER="${SUDO_USER:-${USER:-${USERNAME:-}}}"
  if [[ -z "${REAL_USER}" || "${REAL_USER}" == "root" ]]; then
    REAL_USER="$(id -un)"
  fi
fi

if command -v getent >/dev/null 2>&1; then
  REAL_HOME="$(getent passwd "${REAL_USER}" | cut -d: -f6)"
elif [[ "${OS_FAMILY}" == darwin ]]; then
  REAL_HOME="$(dscl . -read "/Users/${REAL_USER}" NFSHomeDirectory 2>/dev/null | awk '{print $2}')"
fi
REAL_HOME="${REAL_HOME:-${HOME}}"
REAL_UID="$(id -u "${REAL_USER}" 2>/dev/null || id -u)"
REAL_GID="$(id -g "${REAL_USER}" 2>/dev/null || id -g)"

need_cmd() { command -v "$1" >/dev/null 2>&1; }
have_systemd() {
  need_cmd systemctl || return 1
  systemctl is-system-running >/dev/null 2>&1 || [[ "$(systemctl is-system-running 2>/dev/null || true)" == "degraded" ]]
}

# User-facing PATH (kind/kubectl often land in sbin).
USER_PATH="/usr/local/bin:/usr/local/sbin:/opt/homebrew/bin:/usr/bin:/bin"
as_user() {
  if [[ "$(id -u)" -eq 0 && "${REAL_USER}" != "root" ]]; then
    sudo -u "${REAL_USER}" -- env PATH="${USER_PATH}:${PATH}" "$@"
  else
    env PATH="${USER_PATH}:${PATH}" "$@"
  fi
}
as_user_sh() {
  if [[ "$(id -u)" -eq 0 && "${REAL_USER}" != "root" ]]; then
    sudo -u "${REAL_USER}" -H env PATH="${USER_PATH}:${PATH}" bash -lc "$*"
  else
    env PATH="${USER_PATH}:${PATH}" bash -lc "$*"
  fi
}

# --- profiles / enablers ----------------------------------------------------
PROFILE="${PROFILE:-full}"
# Back-compat SKIP_* (older docs/agents)
if [[ "${SKIP_KIND:-0}" == "1" ]]; then ENABLE_KIND=0; fi
if [[ "${SKIP_FULL_IMAGES:-0}" == "1" ]]; then ENABLE_FULL_IMAGES=0; fi
if [[ "${SKIP_HARNESS:-0}" == "1" ]]; then
  ENABLE_HARNESS_OPENCODE=0
  ENABLE_HARNESS_PI=0
  ENABLE_HARNESS_CODEX=0
  ENABLE_HARNESS_GROK=0
fi
# ENABLE_LAB is a documented alias for ENABLE_SANDBOX_LAB.
if [[ -z "${ENABLE_SANDBOX_LAB:-}" && -n "${ENABLE_LAB:-}" ]]; then
  ENABLE_SANDBOX_LAB="${ENABLE_LAB}"
fi

set_default() {
  # set_default VAR value  — only if VAR is unset/empty
  eval "cur=\${$1-}"
  if [[ -z "${cur}" ]]; then
    eval "$1=\"\$2\""
  fi
}

case "${PROFILE}" in
  full)
    set_default ENABLE_SANDBOX_LAB 1
    set_default ENABLE_HARNESS_OPENCODE 1
    set_default ENABLE_HARNESS_PI 1
    set_default ENABLE_HARNESS_CODEX 1
    set_default ENABLE_HARNESS_GROK 1
    set_default ENABLE_FULL_IMAGES 1
    set_default ENABLE_KIND 1
    set_default ENABLE_OLLAMA 1
    set_default ENABLE_LMSTUDIO_PROXY 1
    set_default ENABLE_GITSSH 1
    set_default ENABLE_BROWSER_E2E 1
    ;;
  lab)
    set_default ENABLE_SANDBOX_LAB 1
    set_default ENABLE_HARNESS_OPENCODE 1
    set_default ENABLE_HARNESS_PI 1
    set_default ENABLE_HARNESS_CODEX 0
    set_default ENABLE_HARNESS_GROK 0
    set_default ENABLE_FULL_IMAGES 0
    set_default ENABLE_KIND 0
    set_default ENABLE_OLLAMA 1
    set_default ENABLE_LMSTUDIO_PROXY 1
    set_default ENABLE_GITSSH 1
    set_default ENABLE_BROWSER_E2E 0
    ;;
  minimal)
    set_default ENABLE_SANDBOX_LAB 1
    set_default ENABLE_HARNESS_OPENCODE 1
    set_default ENABLE_HARNESS_PI 0
    set_default ENABLE_HARNESS_CODEX 0
    set_default ENABLE_HARNESS_GROK 0
    set_default ENABLE_FULL_IMAGES 0
    set_default ENABLE_KIND 0
    set_default ENABLE_OLLAMA 1
    set_default ENABLE_LMSTUDIO_PROXY 0
    set_default ENABLE_GITSSH 0
    set_default ENABLE_BROWSER_E2E 0
    ;;
  *)
    echo "ERROR: PROFILE=${PROFILE} (want full|lab|minimal)" >&2
    exit 1
    ;;
esac

# Git Bash + kind is possible but often OOMs under Docker Desktop defaults.
if [[ "${OS_FAMILY}" == windows ]]; then
  set_default ENABLE_KIND 0
fi

FORCE_REBUILD="${FORCE_REBUILD:-0}"
OLLAMA_MODEL="${OLLAMA_MODEL:-llama3.2:1b}"
KIND_CLUSTER="${KIND_CLUSTER:-agent-fleet}"

if [[ "${ENABLE_KIND}" == "1" && "${ENABLE_FULL_IMAGES}" != "1" ]]; then
  echo "ERROR: ENABLE_KIND=1 requires ENABLE_FULL_IMAGES=1 (kind loads api/ui/operator)." >&2
  exit 1
fi

# Rust toolchain = workspace rust-version (no python required yet).
if [[ -z "${RUST_TOOLCHAIN:-}" ]]; then
  RUST_TOOLCHAIN="$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "${REPO}/Cargo.toml" | head -1)"
fi
if [[ -z "${RUST_TOOLCHAIN}" ]]; then
  echo "ERROR: could not read rust-version from ${REPO}/Cargo.toml (set RUST_TOOLCHAIN=)" >&2
  exit 1
fi
RUST_IMAGE="${RUST_IMAGE:-rust:${RUST_TOOLCHAIN}-bookworm}"

TMP_ROOT="${REPO}/.tmp"
mkdir -p "${TMP_ROOT}"
chown "${REAL_UID}:${REAL_GID}" "${TMP_ROOT}" 2>/dev/null || true
chmod 775 "${TMP_ROOT}" 2>/dev/null || true

case "$(uname -m)" in
  x86_64|amd64) ARCH=amd64 ;;
  aarch64|arm64) ARCH=arm64 ;;
  *) echo "ERROR: unsupported arch $(uname -m)" >&2; exit 1 ;;
esac

have_image() { docker image inspect "$1" >/dev/null 2>&1; }

docker_build_if_needed() {
  local tag="$1"
  shift
  if [[ "${FORCE_REBUILD}" != "1" ]] && have_image "${tag}"; then
    echo "==> skip ${tag} (exists; FORCE_REBUILD=1 to rebuild)"
    return 0
  fi
  echo "==> docker build ${tag}"
  docker build -t "${tag}" "$@"
}

dc() {
  if docker compose version >/dev/null 2>&1; then
    docker compose "$@"
  elif need_cmd docker-compose; then
    docker-compose "$@"
  else
    echo "ERROR: docker compose plugin missing" >&2
    return 1
  fi
}

echo "==> install  user=${REAL_USER} home=${REAL_HOME} os=${OS_FAMILY} arch=${ARCH}"
echo "    repo=${REPO}"
echo "    PROFILE=${PROFILE}  FORCE_REBUILD=${FORCE_REBUILD}"
echo "    ENABLE sandbox_lab=${ENABLE_SANDBOX_LAB} opencode=${ENABLE_HARNESS_OPENCODE} pi=${ENABLE_HARNESS_PI} codex=${ENABLE_HARNESS_CODEX} grok=${ENABLE_HARNESS_GROK}"
echo "    ENABLE full_images=${ENABLE_FULL_IMAGES} kind=${ENABLE_KIND} ollama=${ENABLE_OLLAMA} proxy1234=${ENABLE_LMSTUDIO_PROXY} gitssh=${ENABLE_GITSSH} browser_e2e=${ENABLE_BROWSER_E2E}"
echo "    rust-version=${RUST_TOOLCHAIN}  image=${RUST_IMAGE}  ollama_model=${OLLAMA_MODEL}"
echo "    tmp=${TMP_ROOT}"

# Fail fast: Docker rust tags / ARG RUST_VERSION must match the toolchain.
mismatch=0
while IFS= read -r df; do
  [[ -f "${df}" ]] || continue
  arg_ver="$(sed -n 's/^ARG[[:space:]]\{1,\}RUST_VERSION=//p' "${df}" | head -1 || true)"
  if [[ -n "${arg_ver}" && "${arg_ver}" != "${RUST_TOOLCHAIN}" ]]; then
    echo "ERROR: ${df} has ARG RUST_VERSION=${arg_ver} but rust-version/RUST_TOOLCHAIN=${RUST_TOOLCHAIN}" >&2
    mismatch=1
  fi
  from="$(grep -E '^FROM[[:space:]]+rust:' "${df}" | head -1 || true)"
  [[ -n "${from}" ]] || continue
  if [[ "${from}" == *'${RUST_VERSION}'* ]]; then
    continue
  fi
  if [[ "${from}" != *"${RUST_TOOLCHAIN}"* ]]; then
    echo "ERROR: ${df} has '${from}' but rust-version/RUST_TOOLCHAIN=${RUST_TOOLCHAIN}" >&2
    mismatch=1
  fi
done < <(printf '%s\n' \
  "${REPO}/Dockerfile" \
  "${REPO}/Dockerfile.ui" \
  "${REPO}/Dockerfile.operator" \
  "${REPO}/crates/fleet-sandbox-driver/sandbox-lab/driver/Dockerfile")
if [[ "${mismatch}" -ne 0 ]]; then
  echo "       align FROM rust:<ver> / ARG RUST_VERSION with workspace rust-version," >&2
  echo "       or set RUST_TOOLCHAIN= / RUST_IMAGE=" >&2
  exit 1
fi

# --- packages ---------------------------------------------------------------
# Agent: extend this case when adding a distro; do not fork the script.
install_packages() {
  if [[ "${OS_FAMILY}" == darwin ]]; then
    if ! need_cmd brew && ! as_user command -v brew >/dev/null 2>&1; then
      echo "ERROR: Homebrew not found. Install https://brew.sh then re-run (no root brew)." >&2
      exit 1
    fi
    echo "==> brew install (as ${REAL_USER})"
    as_user_sh "brew install git python3 curl wget jq socat cmake pkg-config openssl libpq || true"
    as_user_sh "brew install kind kubectl || true"
    return 0
  fi

  if [[ "${OS_FAMILY}" == windows ]]; then
    echo "==> Windows/Git Bash: no package manager step."
    echo "    Need on PATH: docker git python3 curl ssh. Install Docker Desktop + Git for Windows."
    echo "    WSL2 Ubuntu is the supported Windows path (then this script is Linux)."
    return 0
  fi

  if need_cmd dnf; then
    pkgs=""
    rpm -q moby-engine >/dev/null 2>&1 || rpm -q docker >/dev/null 2>&1 || pkgs="${pkgs} moby-engine"
    rpm -q docker-cli >/dev/null 2>&1 || pkgs="${pkgs} docker-cli"
    for p in git python3 python3-pip python3-devel python3-redis curl wget \
             openssl openssl-devel pkgconf-pkg-config gcc gcc-c++ make cmake \
             jq socat acl openssh-clients postgresql libpq-devel; do
      rpm -q "$p" >/dev/null 2>&1 || pkgs="${pkgs} $p"
    done
    rpm -q redis >/dev/null 2>&1 || rpm -q valkey-compat-redis >/dev/null 2>&1 || pkgs="${pkgs} valkey-compat-redis"
    if ! docker compose version >/dev/null 2>&1 && ! need_cmd docker-compose; then
      pkgs="${pkgs} docker-compose docker-compose-plugin"
    fi
    if [[ -n "${pkgs}" ]]; then
      echo "==> dnf install:${pkgs}"
      # shellcheck disable=SC2086
      dnf install -y --skip-unavailable ${pkgs}
    else
      echo "==> dnf: required rpms already installed"
    fi
  elif need_cmd apt-get; then
    apt-get update -y
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
      docker.io docker-compose-v2 \
      git python3 python3-pip python3-dev python3-venv python3-redis \
      curl wget openssl pkg-config libssl-dev libpq-dev \
      build-essential cmake make gcc g++ \
      jq socat acl openssh-client postgresql-client \
      redis-server || \
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
      docker.io docker-compose \
      git python3 python3-pip python3-dev \
      curl wget openssl pkg-config libssl-dev \
      build-essential cmake jq socat openssh-client
  elif need_cmd pacman; then
    pacman -Sy --noconfirm --needed \
      docker docker-compose git python python-pip python-redis \
      curl wget openssl pkgconf gcc make cmake jq socat openssh postgresql-libs
  elif need_cmd zypper; then
    zypper --non-interactive install -y \
      docker docker-compose git python3 python3-pip python3-devel \
      curl wget openssl libopenssl-devel gcc gcc-c++ make cmake jq socat \
      openssh postgresql-devel || true
  elif need_cmd apk; then
    apk add --no-cache \
      docker docker-cli-compose git python3 py3-pip py3-redis \
      curl wget openssl pkgconf build-base cmake jq socat openssh \
      postgresql-client
  else
    echo "ERROR: no known package manager (dnf/apt/pacman/zypper/apk/brew)." >&2
    echo "       Install docker, git, python3, curl, gcc, make, jq, socat, openssh manually." >&2
    exit 1
  fi
}

install_packages

if ! need_cmd docker; then
  echo "ERROR: docker CLI missing. Install Docker Engine / Docker Desktop / Colima, then re-run." >&2
  exit 1
fi
if ! docker info >/dev/null 2>&1; then
  echo "ERROR: docker daemon not reachable (start Docker Desktop / sudo systemctl start docker)." >&2
  exit 1
fi
dc version >/dev/null
echo "==> docker ok"

if [[ "${OS_FAMILY}" == linux ]] && [[ "$(id -u)" -eq 0 ]]; then
  if need_cmd systemctl; then
    systemctl enable --now docker 2>/dev/null || true
  fi
  getent group docker >/dev/null 2>&1 || groupadd docker 2>/dev/null || true
  usermod -aG docker "${REAL_USER}" 2>/dev/null || true
  if need_cmd setfacl && [[ -S /var/run/docker.sock ]]; then
    setfacl -m "u:${REAL_USER}:rw" /var/run/docker.sock || true
  fi
  # Do not chmod 666 docker.sock (world-writable = root). Group + ACL is enough.
  # New shells need `newgrp docker` / re-login if only usermod ran.
fi

# --- kubectl + kind (upstream binaries; brew already tried on mac) ----------
# Stage downloads under ${TMP_ROOT} (never sticky system /tmp).
install_upstream_bin() {
  local name="$1" url="$2"
  local staged="${TMP_ROOT}/${name}.$$"
  curl -fsSL -o "${staged}" "${url}"
  chmod 755 "${staged}"
  if [[ "$(id -u)" -eq 0 ]]; then
    mv "${staged}" "/usr/local/bin/${name}"
  else
    mkdir -p "${REAL_HOME}/.local/bin"
    mv "${staged}" "${REAL_HOME}/.local/bin/${name}"
    USER_PATH="${REAL_HOME}/.local/bin:${USER_PATH}"
  fi
}
if ! need_cmd kubectl; then
  echo "==> installing kubectl → /usr/local/bin"
  k8s_os=linux
  [[ "${OS_FAMILY}" == darwin ]] && k8s_os=darwin
  [[ "${OS_FAMILY}" == windows ]] && k8s_os=windows
  k8s_bin=kubectl
  [[ "${OS_FAMILY}" == windows ]] && k8s_bin=kubectl.exe
  install_upstream_bin kubectl \
    "https://dl.k8s.io/release/$(curl -fsSL https://dl.k8s.io/release/stable.txt)/bin/${k8s_os}/${ARCH}/${k8s_bin}"
fi
if [[ "${ENABLE_KIND}" == "1" ]] && ! need_cmd kind; then
  echo "==> installing kind → /usr/local/bin"
  kind_os=linux
  [[ "${OS_FAMILY}" == darwin ]] && kind_os=darwin
  [[ "${OS_FAMILY}" == windows ]] && kind_os=windows
  install_upstream_bin kind "https://kind.sigs.k8s.io/dl/latest/kind-${kind_os}-${ARCH}"
fi
for bin in kubectl kind; do
  src="$(command -v "${bin}" || true)"
  if [[ -n "${src}" && "$(id -u)" -eq 0 && "${src}" != /usr/local/bin/"${bin}" && -d /usr/local/bin ]]; then
    ln -sfn "${src}" "/usr/local/bin/${bin}" 2>/dev/null || true
  fi
done
as_user kubectl version --client=true || true
if [[ "${ENABLE_KIND}" == "1" ]]; then
  as_user kind version || true
fi

# --- rustc ------------------------------------------------------------------
echo "==> rust toolchain ${RUST_TOOLCHAIN} for ${REAL_USER}"
if ! as_user_sh "command -v rustup >/dev/null"; then
  as_user_sh "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain ${RUST_TOOLCHAIN}"
fi
as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; rustup toolchain install ${RUST_TOOLCHAIN} && rustup default ${RUST_TOOLCHAIN} && rustc --version && cargo --version"

echo "==> preflight: Cargo.lock vs rustc ${RUST_TOOLCHAIN}"
PRE_DIR="${TMP_ROOT}/install-preflight"
mkdir -p "${PRE_DIR}"
chown -R "${REAL_UID}:${REAL_GID}" "${PRE_DIR}" 2>/dev/null || true
chmod 775 "${PRE_DIR}" 2>/dev/null || true
META_JSON="${PRE_DIR}/metadata.json"
META_ERR="${PRE_DIR}/metadata.err"
set +e
as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; cd \"${REPO}\" && cargo metadata --locked --format-version 1 --offline >'${META_JSON}' 2>'${META_ERR}'"
meta_rc=$?
if [[ "${meta_rc}" -ne 0 ]]; then
  as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; cd \"${REPO}\" && cargo metadata --locked --format-version 1 >'${META_JSON}' 2>'${META_ERR}'"
  meta_rc=$?
fi
set -e
if [[ "${meta_rc}" -ne 0 || ! -s "${META_JSON}" ]]; then
  echo "ERROR: cargo metadata --locked failed (lockfile / rustc ${RUST_TOOLCHAIN}):" >&2
  cat "${META_ERR}" >&2 || true
  exit 1
fi
set +e
FLEET_MSRV="${RUST_TOOLCHAIN}" FLEET_RUST_IMAGE="${RUST_IMAGE}" python3 - "${META_JSON}" <<'PY'
import json, os, sys

msrv = os.environ["FLEET_MSRV"].lstrip("v")
image = os.environ.get("FLEET_RUST_IMAGE", "")

def tup(s):
    parts = []
    for p in str(s).lstrip("v").split("."):
        if p.isdigit():
            parts.append(int(p))
        else:
            break
    while len(parts) < 3:
        parts.append(0)
    return tuple(parts[:3])

want = tup(msrv)
data = json.load(open(sys.argv[1]))
bad = []
for p in data.get("packages", []):
    rv = p.get("rust_version")
    if rv and tup(rv) > want:
        bad.append("  %s@%s requires rustc %s" % (p["name"], p["version"], rv))
if bad:
    sys.stderr.write("ERROR: Cargo.lock needs rustc newer than %s (image %s):\n" % (msrv, image))
    sys.stderr.write("\n".join(bad) + "\n")
    sys.stderr.write(
        "\nEither bump workspace rust-version + Docker FROM rust:<ver>,\n"
        "or select compatible crate versions (rust-version <= %s).\n" % msrv
    )
    sys.exit(1)
print("    lockfile ok: no crate rust-version > %s" % msrv)
PY
pre_py=$?
set -e
[[ "${pre_py}" -eq 0 ]] || exit "${pre_py}"

if ! as_user python3 -c "import redis" >/dev/null 2>&1; then
  echo "==> python redis module missing; trying pip --user"
  as_user_sh "python3 -m pip install --user redis" >/dev/null 2>&1 \
    || as_user_sh "python3 -m pip install --user --break-system-packages redis" >/dev/null 2>&1 \
    || true
fi
if ! as_user python3 -c "import redis" >/dev/null 2>&1; then
  echo "ERROR: python3 cannot import redis (install python3-redis / pip install redis)" >&2
  exit 1
fi

# --- headless Chromium (Playwright) ----------------------------------------
if [[ "${ENABLE_BROWSER_E2E}" == "1" ]]; then
  PW_BROWSERS="${TMP_ROOT}/ms-playwright"
  mkdir -p "${PW_BROWSERS}"
  chown -R "${REAL_UID}:${REAL_GID}" "${PW_BROWSERS}" 2>/dev/null || true
  echo "==> playwright + chromium → ${PW_BROWSERS}"
  if ! as_user python3 -c "from playwright.sync_api import sync_playwright" >/dev/null 2>&1; then
    as_user_sh "python3 -m pip install --user playwright" \
      || as_user_sh "python3 -m pip install --user --break-system-packages playwright" \
      || true
  fi
  if ! as_user python3 -c "from playwright.sync_api import sync_playwright" >/dev/null 2>&1; then
    echo "ERROR: python3 cannot import playwright (pip install playwright)" >&2
    exit 1
  fi
  # Headless Chromium needs OS libs (nss, atk, gtk, gbm, asound, …) that
  # `playwright install chromium` does not install.
  if [[ "${OS_FAMILY}" == linux && "$(id -u)" -eq 0 ]]; then
    echo "==> playwright OS libraries (install-deps chromium)"
    USER_SITE="$(as_user python3 -c 'import site; print(site.getusersitepackages())')"
    if ! PYTHONPATH="${USER_SITE}${PYTHONPATH:+:${PYTHONPATH}}" python3 -m playwright install-deps chromium; then
      echo "ERROR: playwright install-deps chromium failed." >&2
      echo "       Headless Chromium needs nss, atk, gtk, gbm, asound, and related libs." >&2
      exit 1
    fi
  elif [[ "${OS_FAMILY}" == linux ]]; then
    echo "WARN: not root; skipping playwright install-deps. Chromium may fail without nss/atk/gbm/asound." >&2
  fi
  as_user_sh "PLAYWRIGHT_BROWSERS_PATH='${PW_BROWSERS}' python3 -m playwright install chromium"
  echo "==> playwright chromium headless smoke"
  if ! as_user_sh "PLAYWRIGHT_BROWSERS_PATH='${PW_BROWSERS}' python3 -c 'from playwright.sync_api import sync_playwright; p = sync_playwright().start(); b = p.chromium.launch(headless=True); b.close(); p.stop()'"; then
    echo "ERROR: playwright chromium headless launch failed." >&2
    echo "       Missing OS libraries (nss, atk, gtk, gbm, asound, …) or browser files under ${PW_BROWSERS}." >&2
    echo "       Re-run this script as root with ENABLE_BROWSER_E2E=1." >&2
    exit 1
  fi
fi

# --- ollama -----------------------------------------------------------------
ollama_listen() {
  if need_cmd ss; then
    ss -ltn 2>/dev/null | awk '/:11434/ {print $4; exit}'
  elif need_cmd netstat; then
    netstat -an 2>/dev/null | awk '/11434/ && /LISTEN/ {print $4; exit}'
  fi
}

if [[ "${ENABLE_OLLAMA}" == "1" ]]; then
  if ! need_cmd ollama && ! as_user command -v ollama >/dev/null 2>&1; then
    echo "==> installing ollama"
    if [[ "${OS_FAMILY}" == darwin ]]; then
      as_user_sh "brew install ollama || brew install --cask ollama || true"
    elif [[ "${OS_FAMILY}" == windows ]]; then
      echo "ERROR: install Ollama from https://ollama.com/download then re-run (or use WSL2)." >&2
      exit 1
    else
      curl -fsSL https://ollama.com/install.sh | sh
    fi
  fi

  LISTEN_NOW="$(ollama_listen || true)"
  case "${LISTEN_NOW}" in
    0.0.0.0:11434|\[::\]:11434|\*:11434) ;;
    *)
      if [[ "${OS_FAMILY}" == linux ]]; then
        pkill -x ollama 2>/dev/null || true
        sleep 0.5
      fi
      ;;
  esac

  if have_systemd && [[ "${OS_FAMILY}" == linux ]]; then
    mkdir -p /etc/systemd/system/ollama.service.d
    cat >/etc/systemd/system/ollama.service.d/fleet-lab.conf <<'EOF'
[Service]
Environment="OLLAMA_HOST=0.0.0.0:11434"
EOF
    if systemctl cat ollama.service >/dev/null 2>&1; then
      systemctl daemon-reload
      systemctl enable --now ollama
      systemctl restart ollama
    else
      echo "==> no ollama.service; setsid ollama serve as ${REAL_USER}"
      as_user_sh "cd / && setsid -f env OLLAMA_HOST=0.0.0.0:11434 ollama serve >'${TMP_ROOT}/ollama-serve.log' 2>&1"
      sleep 2
    fi
  else
    echo "==> starting detached ollama serve (no systemd) as ${REAL_USER}"
    # macOS brew services often ignore OLLAMA_HOST — prefer setsid serve.
    as_user_sh "cd / && (command -v setsid >/dev/null && setsid -f env OLLAMA_HOST=0.0.0.0:11434 ollama serve >'${TMP_ROOT}/ollama-serve.log' 2>&1) || (nohup env OLLAMA_HOST=0.0.0.0:11434 ollama serve >'${TMP_ROOT}/ollama-serve.log' 2>&1 &)"
    sleep 2
  fi

  for _ in $(seq 1 40); do
    curl -sf --max-time 1 http://127.0.0.1:11434/api/tags >/dev/null && break
    sleep 0.5
  done
  curl -sf http://127.0.0.1:11434/api/tags >/dev/null

  LISTEN="$(ollama_listen || true)"
  echo "==> ollama listen ${LISTEN:-unknown}"
  case "${LISTEN}" in
    127.0.0.1:*|\[::1\]:*)
      echo "ERROR: ollama still loopback-only; containers cannot use host.docker.internal:11434" >&2
      exit 1
      ;;
  esac

  echo "==> pulling ollama model ${OLLAMA_MODEL}"
  as_user ollama pull "${OLLAMA_MODEL}" || as_user_sh "ollama pull ${OLLAMA_MODEL}"
  curl -sf http://127.0.0.1:11434/v1/models | head -c 120 || true
  echo
fi

# --- :1234 OpenAI-compat proxy ----------------------------------------------
if [[ "${ENABLE_LMSTUDIO_PROXY}" == "1" ]]; then
  if ! need_cmd socat && ! as_user command -v socat >/dev/null 2>&1; then
    echo "ERROR: socat missing (needed for ENABLE_LMSTUDIO_PROXY=1). Install socat or set ENABLE_LMSTUDIO_PROXY=0." >&2
    exit 1
  fi
  SOCAT_BIN="$(command -v socat || true)"
  [[ -n "${SOCAT_BIN}" ]] || SOCAT_BIN="$(as_user command -v socat)"
  if have_systemd && [[ "${OS_FAMILY}" == linux ]]; then
    cat >/etc/systemd/system/fleet-openai-compat-1234.service <<EOF
[Unit]
Description=Agent Fleet OpenAI-compat proxy :1234 -> Ollama :11434
After=network.target

[Service]
ExecStart=${SOCAT_BIN} TCP-LISTEN:1234,fork,reuseaddr,bind=0.0.0.0 TCP:127.0.0.1:11434
Restart=always
RestartSec=1

[Install]
WantedBy=multi-user.target
EOF
    systemctl daemon-reload
    systemctl enable --now fleet-openai-compat-1234.service
  else
    as_user_sh "cd / && (command -v setsid >/dev/null && setsid -f '${SOCAT_BIN}' TCP-LISTEN:1234,fork,reuseaddr,bind=0.0.0.0 TCP:127.0.0.1:11434) || (nohup '${SOCAT_BIN}' TCP-LISTEN:1234,fork,reuseaddr,bind=0.0.0.0 TCP:127.0.0.1:11434 >/dev/null 2>&1 &)"
  fi
  sleep 0.3
  curl -sf --max-time 2 http://127.0.0.1:1234/v1/models | head -c 80 || true
  echo
  echo "==> :1234 -> :11434 proxy"
fi

# --- dummy SSH + optional git-ssh -------------------------------------------
SSH_DIR="${TMP_ROOT}/ssh"
mkdir -p "${SSH_DIR}"
if [[ ! -f "${SSH_DIR}/id_ed25519" ]]; then
  ssh-keygen -t ed25519 -f "${SSH_DIR}/id_ed25519" -N "" -C "fleet-lab-dummy"
fi
ssh-keyscan -t ed25519,rsa,ecdsa github.com 2>/dev/null >"${SSH_DIR}/known_hosts" || true
mkdir -p "${REAL_HOME}/.ssh" 2>/dev/null || true
touch "${REAL_HOME}/.ssh/known_hosts" 2>/dev/null || true
if [[ -s "${SSH_DIR}/known_hosts" && -f "${REAL_HOME}/.ssh/known_hosts" ]]; then
  while IFS= read -r line || [[ -n "${line}" ]]; do
    grep -qxF "$line" "${REAL_HOME}/.ssh/known_hosts" 2>/dev/null || echo "$line" >>"${REAL_HOME}/.ssh/known_hosts"
  done <"${SSH_DIR}/known_hosts"
fi
chown -R "${REAL_UID}:${REAL_GID}" "${SSH_DIR}" 2>/dev/null || true
chown -R "${REAL_UID}:${REAL_GID}" "${REAL_HOME}/.ssh" 2>/dev/null || true
chmod 700 "${SSH_DIR}" 2>/dev/null || true
chmod 600 "${SSH_DIR}/id_ed25519" 2>/dev/null || true
chmod 644 "${SSH_DIR}/id_ed25519.pub" "${SSH_DIR}/known_hosts" 2>/dev/null || true

if [[ "${ENABLE_GITSSH}" == "1" ]]; then
  echo "==> building local git-ssh test server :2222"
  GITSSH_CTX="${TMP_ROOT}/gitssh-build"
  rm -rf "${GITSSH_CTX}"
  mkdir -p "${GITSSH_CTX}"
  cp "${SSH_DIR}/id_ed25519.pub" "${GITSSH_CTX}/authorized_keys"
  cat >"${GITSSH_CTX}/Dockerfile" <<'EOF'
FROM alpine:3.21
RUN apk add --no-cache openssh git \
 && adduser -D -s /bin/sh git \
 && passwd -u git \
 && mkdir -p /home/git/.ssh /srv/git \
 && chmod 700 /home/git/.ssh \
 && ssh-keygen -A
COPY authorized_keys /home/git/.ssh/authorized_keys
RUN chmod 600 /home/git/.ssh/authorized_keys \
 && chown -R git:git /home/git/.ssh \
 && git config --global user.email "lab@example.test" \
 && git config --global user.name "fleet-lab" \
 && mkdir /tmp/seed && cd /tmp/seed \
 && git init -b main \
 && echo hello-from-local-git-ssh > README \
 && git add README && git commit -m init \
 && git clone --bare /tmp/seed /srv/git/hello.git \
 && chown -R git:git /srv/git \
 && printf '%s\n' \
      'PasswordAuthentication no' \
      'PubkeyAuthentication yes' \
      'PermitRootLogin no' \
      'AllowUsers git' \
      > /etc/ssh/sshd_config.d/fleet.conf
EXPOSE 22
CMD ["/usr/sbin/sshd", "-D", "-e"]
EOF
  docker build -t agent-fleet-git-ssh:local "${GITSSH_CTX}"
  rm -rf "${GITSSH_CTX}"
  docker rm -f fleet-git-ssh >/dev/null 2>&1 || true
  docker run -d --name fleet-git-ssh --restart unless-stopped \
    -p 2222:22 \
    agent-fleet-git-ssh:local
  echo "==> git-ssh ssh://git@127.0.0.1:2222/srv/git/hello.git"
fi

# --- lab .env ---------------------------------------------------------------
ENV_FILE="${LAB}/.env"
env_get() {
  local key="$1"
  [[ -f "${ENV_FILE}" ]] || return 0
  grep -E "^${key}=" "${ENV_FILE}" | tail -1 | cut -d= -f2- || true
}
OLD_OPENAI="$(env_get OPENAI_API_KEY)"
OLD_CODEX="$(env_get CODEX_API_KEY)"
OLD_GROK="$(env_get GROK_DEPLOYMENT_KEY)"
OLD_ANTHROPIC="$(env_get ANTHROPIC_API_KEY)"
if [[ -f "${REAL_HOME}/.grok/auth.json" ]]; then
  cp -a "${REAL_HOME}/.grok/auth.json" "${LAB}/.grok-auth.json"
  chown "${REAL_UID}:${REAL_GID}" "${LAB}/.grok-auth.json" 2>/dev/null || true
  echo "==> copied host ~/.grok/auth.json -> sandbox-lab/.grok-auth.json (gitignored)"
fi

cat >"${ENV_FILE}" <<EOF
REDIS_URL=redis://redis:6379/0
REDIS_URL_HOST=redis://127.0.0.1:6379/0
FLEET_JOBS_QUEUE=fleet:jobs
FLEET_RESULTS_QUEUE=fleet:results
HARNESS=opencode
GIT_REPO_URL=https://github.com/eugenepyvovarov/123-invoices.git
GIT_REF=main
GIT_SSH_SMOKE_URL=ssh://git@host.docker.internal:2222/srv/git/hello.git
SSH_KEY_HOST_PATH=${SSH_DIR}/id_ed25519
OPENCODE_PROVIDER_ID=ollama
OPENCODE_PROVIDER_NAME="Ollama (local)"
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
DATABASE_URL=postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable
JWT_SECRET=dev-change-me-to-a-long-random-secret-at-least-32-chars
ADMIN_TOKEN=test-admin
OPENAI_API_KEY=${OLD_OPENAI}
CODEX_API_KEY=${OLD_CODEX}
GROK_DEPLOYMENT_KEY=${OLD_GROK}
ANTHROPIC_API_KEY=${OLD_ANTHROPIC}
EOF
chown "${REAL_UID}:${REAL_GID}" "${ENV_FILE}" 2>/dev/null || true
echo "==> wrote ${ENV_FILE}"

# --- images -----------------------------------------------------------------
echo "==> pulling base images (needed ones only)"
docker pull redis:7-alpine
docker pull python:3.13-bookworm
docker pull alpine:3.21
if [[ "${ENABLE_SANDBOX_LAB}" == "1" || "${ENABLE_FULL_IMAGES}" == "1" ]]; then
  docker pull "${RUST_IMAGE}"
fi
if [[ "${ENABLE_SANDBOX_LAB}" == "1" || "${ENABLE_FULL_IMAGES}" == "1" ]]; then
  docker pull postgres:16-alpine
fi

build_one_harness() {
  local name="$1" file="$2"
  local tag="agent-fleet-${name}:alpine"
  if [[ "${FORCE_REBUILD}" != "1" ]] && have_image "${tag}"; then
    echo "==> skip ${tag}"
    return 0
  fi
  if ! have_image agent-fleet-base:alpine || [[ "${FORCE_REBUILD}" == "1" && "${name}" != base ]]; then
    if [[ "${name}" != base ]]; then
      as_user docker build -t agent-fleet-base:alpine -f "${AGENTS}/base/Dockerfile" "${AGENTS}/base"
    fi
  fi
  if [[ "${name}" == base ]]; then
    as_user docker build -t agent-fleet-base:alpine -f "${AGENTS}/base/Dockerfile" "${AGENTS}/base"
  else
    as_user docker build -t "${tag}" --build-arg BASE_IMAGE=agent-fleet-base:alpine \
      -f "${AGENTS}/${file}/Dockerfile" "${AGENTS}/${file}"
  fi
}

if [[ "${ENABLE_HARNESS_OPENCODE}" == "1" || "${ENABLE_HARNESS_PI}" == "1" \
   || "${ENABLE_HARNESS_CODEX}" == "1" || "${ENABLE_HARNESS_GROK}" == "1" ]]; then
  build_one_harness base base
fi
[[ "${ENABLE_HARNESS_OPENCODE}" == "1" ]] && build_one_harness opencode opencode
[[ "${ENABLE_HARNESS_PI}" == "1" ]] && build_one_harness pi pi
[[ "${ENABLE_HARNESS_CODEX}" == "1" ]] && build_one_harness codex codex
[[ "${ENABLE_HARNESS_GROK}" == "1" ]] && build_one_harness grok grok

if have_image agent-fleet-grok:alpine && [[ "${ENABLE_HARNESS_GROK}" == "1" ]]; then
  if ! docker run --rm --user 1000:1000 --entrypoint /usr/local/bin/grok \
       agent-fleet-grok:alpine --version >"${TMP_ROOT}/grok-version.txt" 2>&1; then
    echo "==> grok image not executable as uid 1000 — rebuilding"
    FORCE_REBUILD=1 build_one_harness grok grok
    if ! docker run --rm --user 1000:1000 --entrypoint /usr/local/bin/grok \
         agent-fleet-grok:alpine --version >"${TMP_ROOT}/grok-version.txt" 2>&1; then
      echo "ERROR: grok image still not executable as fleet user:" >&2
      cat "${TMP_ROOT}/grok-version.txt" >&2 || true
      exit 1
    fi
  fi
  echo "==> grok image ok as uid 1000: $(tr '\n' ' ' <"${TMP_ROOT}/grok-version.txt")"
fi

if [[ "${ENABLE_SANDBOX_LAB}" == "1" ]]; then
  echo "==> building lab workspace + driver"
  (
    cd "${LAB}"
    export DRIVER_BASE_IMAGE=agent-fleet-opencode:alpine
    # compose image: → agent-fleet-sandbox-workspace:local / agent-fleet-sandbox-driver:local
    dc build workspace
    dc build --pull=false driver
  )
  echo "==> host Postgres on :5433"
  (
    cd "${REPO}"
    dc up -d postgres
  )
  pg_ok=0
  for _ in $(seq 1 30); do
    if dc -f "${REPO}/docker-compose.yml" --project-directory "${REPO}" exec -T postgres pg_isready -U fleet -d fleet; then
      pg_ok=1
      break
    fi
    sleep 1
  done
  if [[ "${pg_ok}" -ne 1 ]]; then
    echo "ERROR: postgres on :5433 never became ready" >&2
    dc -f "${REPO}/docker-compose.yml" --project-directory "${REPO}" ps postgres >&2 || true
    exit 1
  fi
fi

if [[ "${ENABLE_FULL_IMAGES}" == "1" ]]; then
  echo "==> full-stack images (api + ui + operator) — several minutes each"
  docker_build_if_needed agent-fleet-api:local -f "${REPO}/Dockerfile" "${REPO}"
  docker_build_if_needed agent-fleet-ui:local -f "${REPO}/Dockerfile.ui" "${REPO}"
  docker_build_if_needed agent-fleet-operator:local -f "${REPO}/Dockerfile.operator" "${REPO}"
fi

if [[ "${ENABLE_KIND}" == "1" ]]; then
  echo "==> kind cluster ${KIND_CLUSTER}"
  missing=""
  for img in agent-fleet-api:local agent-fleet-ui:local agent-fleet-operator:local; do
    have_image "${img}" || missing="${missing} ${img}"
  done
  if [[ -n "${missing}" ]]; then
    echo "ERROR: kind needs images:${missing}" >&2
    exit 1
  fi
  if as_user kind get clusters 2>/dev/null | grep -qx "${KIND_CLUSTER}"; then
    echo "    already exists"
  else
    as_user kind create cluster --name "${KIND_CLUSTER}" --config "${REPO}/infra/kind.yaml"
  fi
  as_user kind load docker-image \
    agent-fleet-api:local \
    agent-fleet-ui:local \
    agent-fleet-operator:local \
    --name "${KIND_CLUSTER}"
  as_user kubectl apply -k "${REPO}/infra/overlays/kind"
  echo "==> waiting for agent-fleet pods"
  if ! as_user kubectl -n agent-fleet wait --for=condition=Ready pods --all --timeout=180s; then
    as_user kubectl -n agent-fleet get pods,svc >&2 || true
    echo "ERROR: kind pods not Ready" >&2
    exit 1
  fi
  # In-cluster NodePort (works even if host publish is weird).
  if docker exec "${KIND_CLUSTER}-control-plane" curl -sf --max-time 5 http://127.0.0.1:30080/health >/dev/null; then
    echo "    kind node :30080 /health ok"
  else
    echo "ERROR: API /health failed from kind node :30080" >&2
    exit 1
  fi
  # Host extraPortMappings (blocked if api-ingress netpol omits node/host).
  if curl -sf --max-time 5 http://127.0.0.1:8080/health >/dev/null; then
    echo "    host :8080 /health ok"
  else
    echo "ERROR: host :8080 /health failed (kind extraPortMappings / NetworkPolicy)." >&2
    echo "       In-cluster API is up; host publish is not. Check overlays/kind/api-nodeport-netpol.yaml" >&2
    exit 1
  fi
  if curl -sf --max-time 5 -o /dev/null http://127.0.0.1:3030/; then
    echo "    host :3030 UI ok"
  else
    echo "ERROR: host :3030 UI failed" >&2
    exit 1
  fi
fi

echo
echo "==> install complete for ${REAL_USER}  os=${OS_FAMILY}  PROFILE=${PROFILE}"
echo "    rustc ${RUST_TOOLCHAIN}  tmp ${TMP_ROOT}  env ${ENV_FILE}"
[[ "${ENABLE_OLLAMA}" == "1" ]] && echo "    ollama ${OLLAMA_MODEL}"
[[ "${ENABLE_LMSTUDIO_PROXY}" == "1" ]] && echo "    proxy :1234 -> :11434"
[[ "${ENABLE_GITSSH}" == "1" ]] && echo "    git-ssh :2222  key ${SSH_DIR}/id_ed25519"
[[ "${ENABLE_SANDBOX_LAB}" == "1" ]] && echo "    sandbox compose workspace+driver + postgres :5433"
[[ "${ENABLE_FULL_IMAGES}" == "1" ]] && echo "    images agent-fleet-{api,ui,operator}:local"
[[ "${ENABLE_KIND}" == "1" ]] && echo "    kind ${KIND_CLUSTER}"
[[ "${ENABLE_BROWSER_E2E}" == "1" ]] && echo "    playwright chromium  ${TMP_ROOT}/ms-playwright"
echo
echo "Do not run tests here. As ${REAL_USER}:"
echo "  source ~/.cargo/env   # if rustup is new"
echo "  ./crates/fleet-sandbox-driver/sandbox-lab/scripts/run-full-suite.sh"
[[ "${ENABLE_FULL_IMAGES}" == "1" ]] && echo "  DATABASE_URL=postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable cargo test -p fleet-orchestrator"
[[ "${ENABLE_KIND}" == "1" ]] && echo "  kubectl -n agent-fleet get pods"
echo
echo "Re-run (same command, no args): ${SCRIPT_PATH}"
echo "Change PROFILE/ENABLE_* in ${ROOT_ENV} (not on the command line)."
