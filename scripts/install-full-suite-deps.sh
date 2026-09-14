#!/usr/bin/env bash
# =============================================================================
# Agent Fleet — host install (ONLY source of truth for install)
# =============================================================================
# Path: scripts/install-full-suite-deps.sh
# Does NOT run tests. Re-run is safe (skips api/ui/operator :local tags only when
# image label fleet.git_sha matches this HEAD; FORCE_REBUILD=1 always rebuilds).
#
# An agent reading this file should:
#   1) Detect OS / RAM / disk (hints below).
#   2) Persist PROFILE/ENABLE_* in the repo-root .env (same file as the rest of
#      the project). Never pass them on the command line.
#   3) Tell the human to run this script with zero arguments (Linux self-sudos).
#   4) Not invent a second install doc — this file is the doc.
#
# Command (same everywhere — no flags, no PROFILE= prefix):
#   /abs/path/to/scripts/install-full-suite-deps.sh
# Linux elevates itself with sudo. macOS/Git Bash run as the current user
# (Homebrew must not be root; script uses SUDO_USER / $USER for brew).
# Windows: WSL2 Ubuntu (preferred) or Git Bash + Docker Desktop. Not cmd/PS.
#
# -----------------------------------------------------------------------------
# Compatible systems — supported vs tested
# -----------------------------------------------------------------------------
# The script has a package path for each row. "tested" means a human ran this
# file to completion on that OS. Do not mark a row tested from reading code.
#
#   OS / PM                         Status
#   Linux  dnf    Fedora            TESTED (this checkout, 2026-08-09)
#   Linux  dnf    RHEL / CentOS     not tested
#   Linux  apt    Debian / Ubuntu   not tested
#   Linux  pacman Arch / Manjaro    not tested
#   Linux  zypper openSUSE          not tested
#   Linux  apk    Alpine            not tested
#   macOS         Homebrew          not tested
#   Windows WSL2  (as Linux)        not tested
#   Windows Git Bash                not tested (no package manager step)
#   Windows cmd / PowerShell        not supported
#
# When you complete a first install on another OS, change that row to TESTED
# and date it. Keep the OS-matrix section below in sync with the case arms.
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
#   ENABLE_RUNNER_GUEST_E2E    real-life runner guests (VM per OS). Default on
#                              for every PROFILE. Missing guest/stack is a fail.
#                              Explicit 0 still wins. See
#                              docs/components/runners/testing-procedures.md
#   ENABLE_BROWSER_E2E         pip playwright + Chromium under <repo>/.tmp/ms-playwright
#                              + OS libs via this script's matrix (dnf/apt/pacman/
#                              zypper/apk; brew/darwin uses Playwright's own
#                              Chromium). Never call `playwright install-deps`
#                              except on apt hosts — it shells out to apt-get.
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
#                     Playwright libs: same PM (never apt-get on dnf/apk/…).
# Linux, no systemd   same packages; ollama/socat via setsid (not a child of this
#                     script — otherwise bash never exits)
# macOS               Homebrew as the login user (never brew-as-root). Docker
#                     Desktop or Colima must already run. launchd/brew services
#                     or setsid for ollama + :1234 proxy. No docker.sock chmod.
#                     Playwright uses its bundled Chromium (no extra brew libs).
# Windows + WSL2      treat as Linux (install docker-ce or use Docker Desktop
#                     WSL integration).
# Windows + Git Bash  no pacman/apt. Require docker, git, python3, curl, ssh
#                     already on PATH (Docker Desktop). Skip systemd, chown if
#                     it fails, usermod, setfacl. ENABLE_KIND default 0.
#                     ENABLE_BROWSER_E2E: Playwright's win Chromium, or use WSL2.
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
# Repo root from this file: scripts/… or leftover sandbox-lab/scripts/….
_probe="${SCRIPT_DIR}"
REPO=""
while [[ "${_probe}" != "/" ]]; do
  if [[ -f "${_probe}/Cargo.toml" && -d "${_probe}/crates/fleet-sandbox-driver" ]]; then
    REPO="${_probe}"
    break
  fi
  _probe="$(cd "${_probe}/.." && pwd)"
done
if [[ -z "${REPO}" ]]; then
  echo "ERROR: cannot find repo root from ${SCRIPT_PATH}" >&2
  exit 1
fi
LAB="${REPO}/crates/fleet-sandbox-driver/sandbox-lab"
AGENTS="${REPO}/crates/fleet-sandbox-driver/agents"
ROOT_ENV="${REPO}/.env"
# Caller may be in a deleted worktree (/tmp/…). Never inherit that cwd.
# rustup/cargo/docker fail with getcwd if PWD is gone.
if ! cd "${REPO}"; then
  echo "ERROR: cannot cd to ${REPO}" >&2
  exit 1
fi
# Compose interpolates this for every `docker compose` invoke (including
# `up postgres`). Dockerfiles still reject an empty SHA at image build.
export FLEET_GIT_SHA
FLEET_GIT_SHA="$(bash "${REPO}/scripts/tree-id.sh")"

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
# .env must not leave a blank/stale SHA for compose interpolation.
FLEET_GIT_SHA="$(bash "${REPO}/scripts/tree-id.sh")"
export FLEET_GIT_SHA

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
    set_default ENABLE_RUNNER_GUEST_E2E 1
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
    set_default ENABLE_RUNNER_GUEST_E2E 1
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
    set_default ENABLE_RUNNER_GUEST_E2E 1
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

# Registry 502/timeouts are not a missing dep if the tag is already local.
docker_pull_or_have() {
  local ref="$1"
  local i
  for i in 1 2 3; do
    if docker pull "${ref}"; then
      return 0
    fi
    echo "    docker pull ${ref} failed (attempt ${i}/3)"
    sleep $((i * 3))
  done
  if have_image "${ref}"; then
    echo "==> ${ref} already local; registry refresh failed, continuing"
    return 0
  fi
  echo "ERROR: docker pull ${ref} failed and no local image." >&2
  echo "       Re-run this script when the registry is up. Do not retag a random image." >&2
  exit 1
}

docker_build_if_needed() {
  local tag="$1"
  shift
  local sha have
  sha="$(bash "${REPO}/scripts/tree-id.sh")"
  if [[ "${FORCE_REBUILD}" != "1" ]] && have_image "${tag}"; then
    have="$(docker image inspect -f '{{index .Config.Labels "fleet.git_sha"}}' "${tag}" 2>/dev/null || true)"
    if [[ "${have}" == "${sha}" ]]; then
      echo "==> skip ${tag} (fleet.git_sha=${sha})"
      return 0
    fi
    echo "==> rebuild ${tag}: image fleet.git_sha=${have:-unset} != HEAD ${sha}"
  fi
  echo "==> docker build ${tag} FLEET_GIT_SHA=${sha}"
  DOCKER_BUILDKIT=1 docker build -t "${tag}" --build-arg "FLEET_GIT_SHA=${sha}" "$@"
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
    as_user_sh "brew install git python3 curl wget jq socat cmake pkg-config openssl libpq"
    as_user_sh "brew install kind kubectl" || true
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
    rpm -q docker-cli >/dev/null 2>&1 || pkgs="${pkgs} docker-cli"
    for p in git python3 python3-pip python3-devel python3-redis curl wget \
             openssl openssl-devel pkgconf-pkg-config gcc gcc-c++ make cmake \
             jq socat acl openssh-clients postgresql libpq-devel; do
      rpm -q "$p" >/dev/null 2>&1 || pkgs="${pkgs} $p"
    done
    if [[ -n "${pkgs}" ]]; then
      echo "==> dnf install:${pkgs}"
      # shellcheck disable=SC2086
      # Unique NEVRAs only. Do not --skip-unavailable (drops required rpms).
      dnf install -y ${pkgs}
    else
      echo "==> dnf: unique rpms already installed"
    fi
    # Aliases: first name that exists in this distro. Complementary, not a dump.
    _pm_install dnf moby-engine docker
    _pm_install dnf valkey-compat-redis redis
    if ! docker compose version >/dev/null 2>&1 && ! need_cmd docker-compose; then
      _pm_install dnf docker-compose-plugin docker-compose
    fi
  elif need_cmd apt-get; then
    apt-get update -y
    # docker-compose-v2 (bookworm+) or docker-compose (older). Fail if neither.
    if ! DEBIAN_FRONTEND=noninteractive apt-get install -y docker-compose-v2; then
      DEBIAN_FRONTEND=noninteractive apt-get install -y docker-compose
    fi
    DEBIAN_FRONTEND=noninteractive apt-get install -y \
      docker.io \
      git python3 python3-pip python3-dev python3-venv python3-redis \
      curl wget openssl pkg-config libssl-dev libpq-dev \
      build-essential cmake make gcc g++ \
      jq socat acl openssh-client postgresql-client \
      redis-server
  elif need_cmd pacman; then
    pacman -Sy --noconfirm --needed \
      docker docker-compose git python python-pip python-redis \
      curl wget openssl pkgconf gcc make cmake jq socat openssh postgresql-libs
  elif need_cmd zypper; then
    zypper --non-interactive install -y \
      docker docker-compose git python3 python3-pip python3-devel \
      curl wget openssl libopenssl-devel gcc gcc-c++ make cmake jq socat \
      openssh postgresql-devel
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

# Install the first alias this PM accepts. Extra args are the same package
# under another name (Ubuntu t64, openSUSE mozilla-*, Alpine gtk+3.0).
# Complementary packages are separate calls — do not list them as aliases.
_pm_install() {
  local optional=0
  if [[ "${1:-}" == --optional ]]; then
    optional=1
    shift
  fi
  local pm="$1"
  shift
  local name
  [[ "$#" -ge 1 ]] || return 1
  for name in "$@"; do
    case "${pm}" in
      dnf)
        rpm -q "${name}" >/dev/null 2>&1 && return 0
        dnf install -y "${name}" && return 0
        ;;
      apt)
        dpkg -s "${name}" >/dev/null 2>&1 && return 0
        DEBIAN_FRONTEND=noninteractive apt-get install -y "${name}" && return 0
        ;;
      pacman)
        pacman -Q "${name}" >/dev/null 2>&1 && return 0
        pacman -S --noconfirm --needed "${name}" && return 0
        ;;
      zypper)
        rpm -q "${name}" >/dev/null 2>&1 && return 0
        zypper --non-interactive install -y "${name}" && return 0
        ;;
      apk)
        apk info -e "${name}" >/dev/null 2>&1 && return 0
        apk add --no-cache "${name}" && return 0
        ;;
      *)
        echo "ERROR: _pm_install: unknown package manager '${pm}'" >&2
        return 1
        ;;
    esac
  done
  if [[ "${optional}" -eq 1 ]]; then
    echo "    optional not in repos: $*"
    return 0
  fi
  echo "ERROR: could not install any of: $*" >&2
  return 1
}

# OS shared libraries Playwright's bundled Chromium dlopens.
# `python3 -m playwright install-deps` is Debian/Ubuntu only (apt-get).
install_playwright_os_libs() {
  case "${OS_FAMILY}" in
    darwin)
      echo "==> macOS: Playwright ships Chromium frameworks (no apt/dnf libs)"
      return 0
      ;;
    windows)
      echo "==> Windows/Git Bash: no OS-lib step. ENABLE_BROWSER_E2E needs WSL2 if smoke fails."
      return 0
      ;;
    linux) ;;
    *)
      echo "ERROR: playwright OS libs: unsupported OS_FAMILY=${OS_FAMILY}" >&2
      exit 1
      ;;
  esac
  if [[ "$(id -u)" -ne 0 ]]; then
    echo "ERROR: headless Chromium OS libs need root. Re-run this script (it self-sudos)." >&2
    exit 1
  fi

  local pm=""
  if need_cmd apt-get; then
    pm=apt
  elif need_cmd dnf; then
    pm=dnf
  elif need_cmd pacman; then
    pm=pacman
  elif need_cmd zypper; then
    pm=zypper
  elif need_cmd apk; then
    pm=apk
  else
    echo "ERROR: no package manager for Chromium OS libs (dnf/apt/pacman/zypper/apk)." >&2
    exit 1
  fi
  echo "==> playwright OS libraries via ${pm}"

  # Fast path only where install-deps can actually run apt-get.
  if [[ "${pm}" == apt ]]; then
    local user_site
    user_site="$(as_user python3 -c 'import site; print(site.getusersitepackages())')"
    if PYTHONPATH="${user_site}${PYTHONPATH:+:${PYTHONPATH}}" python3 -m playwright install-deps chromium; then
      return 0
    fi
    echo "==> playwright install-deps failed; installing distro packages by name"
  fi

  # Optional: distro chromium pulls many of the same .so files.
  _pm_install --optional "${pm}" chromium chromium-browser chromium-stable

  _pm_install "${pm}" nss mozilla-nss libnss3
  _pm_install "${pm}" nspr mozilla-nspr libnspr4
  _pm_install "${pm}" atk libatk1.0-0t64 libatk1.0-0
  _pm_install "${pm}" at-spi2-atk libatk-bridge2.0-0t64 libatk-bridge2.0-0 at-spi2-core
  _pm_install "${pm}" at-spi2-core libatspi2.0-0t64 libatspi2.0-0 at-spi2-atk
  _pm_install "${pm}" cups-libs libcups2t64 libcups2 cups
  _pm_install "${pm}" libdrm libdrm2
  _pm_install "${pm}" libxkbcommon libxkbcommon0 libxkbcommon-0
  _pm_install "${pm}" libXcomposite libxcomposite libxcomposite1
  _pm_install "${pm}" libXdamage libxdamage libxdamage1
  _pm_install "${pm}" libXfixes libxfixes libxfixes3
  _pm_install "${pm}" libXrandr libxrandr libxrandr2
  _pm_install "${pm}" mesa-libgbm Mesa-libgbm1 mesa-gbm libgbm1
  _pm_install "${pm}" alsa-lib libasound2t64 libasound2
  _pm_install "${pm}" pango libpango-1.0-0
  _pm_install "${pm}" cairo libcairo2
  _pm_install "${pm}" gtk3 gtk+3.0 libgtk-3-0t64 libgtk-3-0
  _pm_install "${pm}" libX11 libx11 libX11-6 libx11-6
  _pm_install "${pm}" libxcb libxcb1
  _pm_install "${pm}" libXext libxext libXext6 libxext6
  _pm_install "${pm}" libXi libxi libXi6 libxi6
  _pm_install "${pm}" dbus-libs libdbus-1-3 dbus-1 dbus
  _pm_install "${pm}" libxshmfence libxshmfence1
  _pm_install "${pm}" liberation-fonts liberation-sans-fonts fonts-liberation \
    ttf-liberation font-liberation ttf-dejavu font-freefont
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
    # Persist docker across reboot. Daemon already passed `docker info`.
    systemctl enable --now docker 2>/dev/null || true
  fi
  getent group docker >/dev/null 2>&1 || groupadd docker
  usermod -aG docker "${REAL_USER}"
  if need_cmd setfacl && [[ -S /var/run/docker.sock ]]; then
    setfacl -m "u:${REAL_USER}:rw" /var/run/docker.sock
  fi
  # Do not chmod 666 docker.sock (world-writable = root). Group + ACL is enough.
  # New shells need `newgrp docker` / re-login if only usermod ran.
  if ! as_user docker info >/dev/null 2>&1; then
    echo "ERROR: ${REAL_USER} cannot talk to docker.sock after group+ACL." >&2
    echo "       Re-login or run: newgrp docker. Do not chmod 666 the socket." >&2
    exit 1
  fi
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
as_user kubectl version --client=true
if [[ "${ENABLE_KIND}" == "1" ]]; then
  as_user kind version
fi

# --- rustc ------------------------------------------------------------------
echo "==> rust toolchain ${RUST_TOOLCHAIN} for ${REAL_USER}"
if ! as_user_sh "command -v rustup >/dev/null"; then
  as_user_sh "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain ${RUST_TOOLCHAIN}"
fi
as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; rustup toolchain install ${RUST_TOOLCHAIN} && rustup default ${RUST_TOOLCHAIN} && rustc --version && cargo --version"

# --- dioxus-cli (dx). Required. No WARN. Kind UI is not a substitute. ----------
# Host OpenSSL 4 cannot build openssl-sys (dioxus-cli 0.7.x). Do not tell the
# developer to cargo-install around that — this function is the only path.
install_dx() {
  local ver out_bin host_ssl
  ver="$(sed -n 's/^dioxus[[:space:]]*=[[:space:]]*"=\?\([^"]*\)".*/\1/p' "${REPO}/Cargo.toml" | head -1)"
  if [[ -z "${ver}" ]]; then
    echo "ERROR: could not read workspace dioxus pin from ${REPO}/Cargo.toml" >&2
    exit 1
  fi
  echo "==> wasm32-unknown-unknown + dioxus-cli ${ver} (dx)"
  as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; rustup target add wasm32-unknown-unknown"
  if as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; command -v dx >/dev/null && dx --version 2>/dev/null | grep -q '${ver}'"; then
    echo "    dx already ${ver}"
    return 0
  fi
  out_bin="$(as_user_sh 'printf %s "${HOME}/.cargo/bin"')"
  mkdir -p "${out_bin}"
  chown "${REAL_UID}:${REAL_GID}" "${out_bin}" 2>/dev/null || true
  host_ssl="$(openssl version 2>/dev/null || true)"
  # openssl-sys accepts 1.0.2–1.1.1 or 3.x only. Skip the doomed host compile.
  if [[ "${host_ssl}" != *OpenSSL\ 4* && "${host_ssl}" != *OpenSSL\ 5* ]]; then
    echo "==> cargo install dioxus-cli --version ${ver} --locked"
    if as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; cargo install dioxus-cli --version ${ver} --locked" \
      && as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; command -v dx >/dev/null && dx --version 2>/dev/null | grep -q '${ver}'"; then
      echo "    dx $(as_user_sh 'source "$HOME/.cargo/env" 2>/dev/null; dx --version')"
      return 0
    fi
    echo "==> host cargo install dx failed; falling back to ${RUST_IMAGE}"
  else
    echo "==> host ${host_ssl} cannot build openssl-sys; installing dx inside ${RUST_IMAGE}"
  fi
  # Root in the image so PATH has /usr/local/cargo/bin. bash -l as uid 1000
  # drops that PATH ("cargo: command not found"). Copy dx out and chown.
  if ! docker run --rm \
    -v "${out_bin}:/out" \
    -e PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    -e RUSTUP_HOME=/usr/local/rustup \
    -e CARGO_HOME=/usr/local/cargo \
    "${RUST_IMAGE}" \
    bash -c "set -euo pipefail
      command -v cargo
      cargo --version
      cargo install dioxus-cli --version ${ver} --locked
      install -m 755 /usr/local/cargo/bin/dx /out/dx
      chown ${REAL_UID}:${REAL_GID} /out/dx"; then
    echo "ERROR: ${RUST_IMAGE} could not install dx ${ver}. Host dx is required; Kind UI is not a substitute." >&2
    exit 1
  fi
  if ! as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; command -v dx >/dev/null && dx --version 2>/dev/null | grep -q '${ver}'"; then
    echo "ERROR: dx ${ver} not on PATH after ${RUST_IMAGE} install. Re-run this script; do not cargo-install on the host." >&2
    exit 1
  fi
  echo "    dx $(as_user_sh 'source "$HOME/.cargo/env" 2>/dev/null; dx --version')"
}
install_dx
if ! as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; command -v dx >/dev/null"; then
  echo "ERROR: dx not on PATH after install_dx. Kind UI is not a substitute. Missing dx is a fail, not a WARN." >&2
  exit 1
fi

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
  # Headless Chromium needs OS libs (nss, atk, gtk, gbm, asound, …).
  # `python3 -m playwright install-deps` shells out to apt-get (Debian only).
  install_playwright_os_libs
  as_user_sh "PLAYWRIGHT_BROWSERS_PATH='${PW_BROWSERS}' python3 -m playwright install chromium"
  echo "==> playwright chromium headless smoke"
  if ! as_user_sh "PLAYWRIGHT_BROWSERS_PATH='${PW_BROWSERS}' python3 -c 'from playwright.sync_api import sync_playwright; p = sync_playwright().start(); b = p.chromium.launch(headless=True); b.close(); p.stop()'"; then
    echo "ERROR: playwright chromium headless launch failed." >&2
    echo "       Missing OS libraries (nss, atk, gtk, gbm, asound, …) or browser files under ${PW_BROWSERS}." >&2
    echo "       Re-run this script (no flags; ENABLE_BROWSER_E2E=1 in repo-root .env)." >&2
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

# Lab needs 0.0.0.0 / * so compose/kind can use host.docker.internal:11434.
ollama_listen_is_public() {
  case "${1:-}" in
    0.0.0.0:11434|\[::\]:11434|\*:11434) return 0 ;;
    *) return 1 ;;
  esac
}

ollama_tags_ok() {
  curl -sf --max-time 2 http://127.0.0.1:11434/api/tags >/dev/null
}

ollama_write_host_dropin() {
  mkdir -p /etc/systemd/system/ollama.service.d
  cat >/etc/systemd/system/ollama.service.d/fleet-lab.conf <<'EOF'
[Service]
Environment="OLLAMA_HOST=0.0.0.0:11434"
EOF
}

# systemd Restart=always + a leftover `ollama serve` on :11434 hits
# start-limit-hit (hundreds of failed binds). Do not bounce a healthy public
# listener. reset-failed before any start.
ollama_systemd_start() {
  systemctl reset-failed ollama.service 2>/dev/null || true
  systemctl daemon-reload
  systemctl enable ollama.service
  if systemctl start ollama.service; then
    return 0
  fi
  echo "==> ollama.service start failed; reset-failed and retry"
  systemctl reset-failed ollama.service 2>/dev/null || true
  sleep 1
  systemctl start ollama.service
}

ollama_start_detached() {
  echo "==> starting detached ollama serve as ${REAL_USER}"
  as_user_sh "cd / && (command -v setsid >/dev/null && setsid -f env OLLAMA_HOST=0.0.0.0:11434 ollama serve >'${TMP_ROOT}/ollama-serve.log' 2>&1) || (nohup env OLLAMA_HOST=0.0.0.0:11434 ollama serve >'${TMP_ROOT}/ollama-serve.log' 2>&1 &)"
  sleep 2
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
  if ! need_cmd ollama && ! as_user command -v ollama >/dev/null 2>&1; then
    echo "ERROR: ollama is not on PATH after install." >&2
    exit 1
  fi

  LISTEN_NOW="$(ollama_listen || true)"
  if ollama_tags_ok && ollama_listen_is_public "${LISTEN_NOW}"; then
    echo "==> ollama already listening ${LISTEN_NOW} — not restarting"
    if have_systemd && [[ "${OS_FAMILY}" == linux ]] && [[ "$(id -u)" -eq 0 ]]; then
      ollama_write_host_dropin
      systemctl daemon-reload || true
      if systemctl is-failed --quiet ollama.service; then
        systemctl reset-failed ollama.service || true
      fi
    fi
  else
    # Only kill a loopback/dead instance we are about to replace.
    if [[ "${OS_FAMILY}" == linux ]] && ! ollama_listen_is_public "${LISTEN_NOW}"; then
      pkill -x ollama 2>/dev/null || true
      sleep 0.5
    fi
    if have_systemd && [[ "${OS_FAMILY}" == linux ]] && [[ "$(id -u)" -eq 0 ]]; then
      ollama_write_host_dropin
      if systemctl cat ollama.service >/dev/null 2>&1; then
        if ! ollama_systemd_start; then
          echo "==> ollama.service still failed after reset-failed; detached serve"
          ollama_start_detached
        fi
      else
        echo "==> no ollama.service; setsid ollama serve as ${REAL_USER}"
        ollama_start_detached
      fi
    else
      ollama_start_detached
    fi
  fi

  tags_ok=0
  for _ in $(seq 1 40); do
    if ollama_tags_ok; then
      tags_ok=1
      break
    fi
    sleep 0.5
  done
  if [[ "${tags_ok}" -ne 1 ]]; then
    echo "ERROR: ENABLE_OLLAMA=1 but http://127.0.0.1:11434/api/tags is not up." >&2
    if have_systemd && [[ "${OS_FAMILY}" == linux ]]; then
      systemctl status ollama.service --no-pager >&2 || true
    fi
    exit 1
  fi

  LISTEN="$(ollama_listen || true)"
  echo "==> ollama listen ${LISTEN:-unknown}"
  if ! ollama_listen_is_public "${LISTEN}"; then
    echo "ERROR: ollama listen ${LISTEN:-unknown} is not 0.0.0.0/*:11434; containers cannot use host.docker.internal:11434" >&2
    exit 1
  fi

  echo "==> pulling ollama model ${OLLAMA_MODEL}"
  as_user ollama pull "${OLLAMA_MODEL}" || as_user_sh "ollama pull ${OLLAMA_MODEL}"
  if ! curl -sf --max-time 5 http://127.0.0.1:11434/v1/models >/dev/null; then
    echo "ERROR: ollama /api/tags is up but /v1/models failed (needed for :1234 proxy)." >&2
    exit 1
  fi
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
  proxy_ok=0
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    if curl -sf --max-time 2 http://127.0.0.1:1234/v1/models >/dev/null; then
      proxy_ok=1
      break
    fi
    sleep 0.3
  done
  if [[ "${proxy_ok}" -ne 1 ]]; then
    echo "ERROR: ENABLE_LMSTUDIO_PROXY=1 but http://127.0.0.1:1234/v1/models is not up." >&2
    exit 1
  fi
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
# Do not copy host ~/.grok/auth.json into the repo (live OIDC + email).
# Grok CLI / fleet-server already read REAL_HOME/.grok/auth.json in place.
rm -f "${LAB}/.grok-auth.json"

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
docker_pull_or_have redis:7-alpine
docker_pull_or_have python:3.13-bookworm
docker_pull_or_have alpine:3.21
if [[ "${ENABLE_SANDBOX_LAB}" == "1" || "${ENABLE_FULL_IMAGES}" == "1" ]]; then
  docker_pull_or_have "${RUST_IMAGE}"
  docker_pull_or_have postgres:16-alpine
fi

harness_src_hash() {
  local file="$1"
  # base + this harness Dockerfile; skip rebuild only when both match the image.
  cat "${AGENTS}/base/Dockerfile" "${AGENTS}/${file}/Dockerfile" 2>/dev/null | sha256sum | awk '{print $1}'
}

build_one_harness() {
  local name="$1" file="$2"
  local tag="agent-fleet-${name}:alpine"
  local src have
  src="$(harness_src_hash "${file}")"
  if [[ "${FORCE_REBUILD}" != "1" ]] && have_image "${tag}"; then
    have="$(docker image inspect -f '{{index .Config.Labels "fleet.harness_src"}}' "${tag}" 2>/dev/null || true)"
    if [[ -n "${src}" && "${have}" == "${src}" ]]; then
      echo "==> skip ${tag} (fleet.harness_src=${src:0:12}…)"
      return 0
    fi
    echo "==> rebuild ${tag}: fleet.harness_src=${have:-unset} != Dockerfile hash"
  fi
  if ! have_image agent-fleet-base:alpine || [[ "${FORCE_REBUILD}" == "1" && "${name}" != base ]]; then
    if [[ "${name}" != base ]]; then
      as_user docker build --label "fleet.harness_src=$(harness_src_hash base)" \
        -t agent-fleet-base:alpine -f "${AGENTS}/base/Dockerfile" "${AGENTS}/base"
    fi
  fi
  if [[ "${name}" == base ]]; then
    as_user docker build --label "fleet.harness_src=${src}" \
      -t agent-fleet-base:alpine -f "${AGENTS}/base/Dockerfile" "${AGENTS}/base"
  else
    as_user docker build --label "fleet.harness_src=${src}" \
      -t "${tag}" --build-arg BASE_IMAGE=agent-fleet-base:alpine \
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
  # Same tag :local + imagePullPolicy IfNotPresent does not recreate pods.
  # `wait Ready` on the old UI/API is how a new image looked installed.
  echo "==> rollout api/ui/operator so nodes run the images just loaded"
  as_user kubectl --context "kind-${KIND_CLUSTER}" -n agent-fleet \
    rollout restart deploy/api deploy/ui deploy/fleet-operator
  if ! as_user kubectl --context "kind-${KIND_CLUSTER}" -n agent-fleet \
        rollout status deploy/api --timeout=300s \
     || ! as_user kubectl --context "kind-${KIND_CLUSTER}" -n agent-fleet \
        rollout status deploy/ui --timeout=300s \
     || ! as_user kubectl --context "kind-${KIND_CLUSTER}" -n agent-fleet \
        rollout status deploy/fleet-operator --timeout=180s; then
    as_user kubectl --context "kind-${KIND_CLUSTER}" -n agent-fleet get pods,svc >&2 || true
    echo "ERROR: kind rollout did not finish" >&2
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
  # Port-up is not this tree — API baked sha AND UI /fleet-git-sha.txt must match HEAD.
  # First probe can time out while the new UI pod takes the extraPortMapping.
  ident_ok=0
  for _ in $(seq 1 20); do
    if as_user python3 "${REPO}/tests/browser/stack_identity.py"; then
      ident_ok=1
      break
    fi
    sleep 3
  done
  if [[ "${ident_ok}" -ne 1 ]]; then
    echo "ERROR: kind host :8080/:3030 is not this tree after install." >&2
    echo "       Rebuild api+ui (install script or ./scripts/run-every-test.sh --prepare-kind)." >&2
    echo "       Do not patch a running pod env to fake identity (sha is baked)." >&2
    exit 1
  fi
fi

echo
if ! as_user_sh "source \"\$HOME/.cargo/env\" 2>/dev/null; command -v dx >/dev/null"; then
  echo "ERROR: dx still not on PATH after install. Refusing to finish green." >&2
  exit 1
fi

echo "==> install complete for ${REAL_USER}  os=${OS_FAMILY}  PROFILE=${PROFILE}"
echo "    rustc ${RUST_TOOLCHAIN}  dx $(as_user_sh 'source "$HOME/.cargo/env" 2>/dev/null; dx --version 2>/dev/null || echo missing')  tmp ${TMP_ROOT}  env ${ENV_FILE}"
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
echo "  ./scripts/human-test.sh          # walk the product (starts API+UI)"
echo "  ./scripts/human-test.sh record   # agent: MR demo .webm (see docs/human-testing.md)"
[[ "${ENABLE_FULL_IMAGES}" == "1" ]] && echo "  DATABASE_URL=postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable cargo test -p fleet-orchestrator"
[[ "${ENABLE_KIND}" == "1" ]] && echo "  kubectl -n agent-fleet get pods"
echo
echo "Re-run (same command, no args): ${SCRIPT_PATH}"
echo "Change PROFILE/ENABLE_* in ${ROOT_ENV} (not on the command line)."
