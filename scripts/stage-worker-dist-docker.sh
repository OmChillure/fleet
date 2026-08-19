#!/usr/bin/env bash
# Dockerized worker-dist builds. Called by stage-worker-dist.sh --bookworm/--musl/--aarch64/--armv7/--aarch64-musl/--armv7-musl/--windows/--windows-arm64
# or run directly:
#   scripts/stage-worker-dist-docker.sh bookworm
#   scripts/stage-worker-dist-docker.sh musl
#   scripts/stage-worker-dist-docker.sh aarch64
#   scripts/stage-worker-dist-docker.sh armv7
#   scripts/stage-worker-dist-docker.sh aarch64-musl
#   scripts/stage-worker-dist-docker.sh armv7-musl
#   scripts/stage-worker-dist-docker.sh windows
#   scripts/stage-worker-dist-docker.sh windows-arm64
#
# bookworm: rust:1.88-bookworm (glibc 2.36) → linux-x86_64-bookworm and
#           linux-x86_64 (portable glibc artifact).
# musl:     rust:1.88-alpine, else rust:1.88-bookworm + musl-tools
#           → linux-x86_64-musl.
# aarch64:  rust:1.88-bookworm + gcc-aarch64-linux-gnu → linux-aarch64
#           (portable glibc ≤ 2.36; isolated target/docker-aarch64-gnu).
# armv7:    rust:1.88-bookworm + gcc-arm-linux-gnueabihf → linux-armv7
#           (portable glibc ≤ 2.36; isolated target/docker-armv7-gnu).
# aarch64-musl: rust:1.88-alpine + rustup target aarch64-unknown-linux-musl
#           (isolated target/docker-aarch64-musl), else bookworm + musl cross.
# armv7-musl: rust:1.88-alpine + rustup target armv7-unknown-linux-musleabihf
#           (isolated target/docker-armv7-musl), else bookworm + musl cross.
# windows:  rust:1.88-bookworm + gcc-mingw-w64 → windows-x86_64 (.exe)
#           (x86_64-pc-windows-gnu; isolated target/docker-windows-gnu).
# windows-arm64: rust:1.88-bookworm + llvm-mingw → windows-arm64 (.exe)
#           (aarch64-pc-windows-gnullvm; isolated target/docker-windows-arm64-gnu).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DEST="${FLEET_WORKER_DIST_DIR:-.tmp/vms/dist}"
# Isolated cargo home so this can run next to a host cargo build.
CARGO_DOCKER="${FLEET_STAGE_CARGO_HOME:-$ROOT/.tmp/cargo-docker}"
BOOKWORM_TARGET="${FLEET_BOOKWORM_TARGET_DIR:-$ROOT/target/docker-bookworm}"
MUSL_TARGET="${FLEET_MUSL_TARGET_DIR:-$ROOT/target/docker-musl}"
AARCH64_GNU_TARGET="${FLEET_AARCH64_GNU_TARGET_DIR:-$ROOT/target/docker-aarch64-gnu}"
ARMV7_GNU_TARGET="${FLEET_ARMV7_GNU_TARGET_DIR:-$ROOT/target/docker-armv7-gnu}"
AARCH64_MUSL_TARGET="${FLEET_AARCH64_MUSL_TARGET_DIR:-$ROOT/target/docker-aarch64-musl}"
ARMV7_MUSL_TARGET="${FLEET_ARMV7_MUSL_TARGET_DIR:-$ROOT/target/docker-armv7-musl}"
WINDOWS_GNU_TARGET="${FLEET_WINDOWS_GNU_TARGET_DIR:-$ROOT/target/docker-windows-gnu}"
WINDOWS_ARM64_TARGET="${FLEET_WINDOWS_ARM64_TARGET_DIR:-$ROOT/target/docker-windows-arm64-gnu}"
LLVM_MINGW_VER="${FLEET_LLVM_MINGW_VER:-20260616}"
LLVM_MINGW_TARBALL="llvm-mingw-${LLVM_MINGW_VER}-ucrt-ubuntu-22.04-x86_64.tar.xz"
LLVM_MINGW_URL="${FLEET_LLVM_MINGW_URL:-https://github.com/mstorsjo/llvm-mingw/releases/download/${LLVM_MINGW_VER}/${LLVM_MINGW_TARBALL}}"
LLVM_MINGW_CACHE="${FLEET_LLVM_MINGW_CACHE:-$ROOT/.tmp/llvm-mingw}"
BOOKWORM_IMAGE="${FLEET_BOOKWORM_RUST_IMAGE:-rust:1.88-bookworm}"
ALPINE_IMAGE="${FLEET_ALPINE_RUST_IMAGE:-rust:1.88-alpine}"

usage() {
  sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'
}

need_docker() {
  if ! command -v docker >/dev/null 2>&1; then
    echo "stage-worker-dist-docker: docker is required" >&2
    exit 1
  fi
}

copy_bins() {
  local src="$1"
  local out="$2"
  mkdir -p "$out"
  local bin
  for bin in fleet-host fleet-worker fleet-driver; do
    if [[ ! -f "$src/$bin" ]]; then
      echo "stage-worker-dist-docker: missing $src/$bin after cargo build" >&2
      exit 1
    fi
    cp "$src/$bin" "$out/$bin"
    chmod 755 "$out/$bin"
    echo "staged $out/$bin"
  done
}

copy_exe_bins() {
  local src="$1"
  local out="$2"
  mkdir -p "$out"
  local bin
  for bin in fleet-host.exe fleet-worker.exe fleet-driver.exe; do
    if [[ ! -f "$src/$bin" ]]; then
      echo "stage-worker-dist-docker: missing $src/$bin after cargo build" >&2
      exit 1
    fi
    cp "$src/$bin" "$out/$bin"
    chmod 755 "$out/$bin"
    echo "staged $out/$bin"
  done
}

# gnullvm PE imports libunwind.dll from the llvm-mingw aarch64 sysroot.
# Prefer the extracted cache / clang sibling; else pull the file out of the
# cached tarball. Fail closed -- do not stage windows-arm64 without it.
find_libunwind_dll() {
  local clang_dir src d extracted
  if command -v aarch64-w64-mingw32-clang >/dev/null 2>&1; then
    clang_dir="$(dirname "$(command -v aarch64-w64-mingw32-clang)")"
    if [[ -f "$clang_dir/../aarch64-w64-mingw32/bin/libunwind.dll" ]]; then
      src="$(cd "$clang_dir/../aarch64-w64-mingw32/bin" && pwd)/libunwind.dll"
      printf '%s\n' "$src"
      return 0
    fi
  fi
  extracted="$LLVM_MINGW_CACHE/llvm-mingw-${LLVM_MINGW_VER}-ucrt-ubuntu-22.04-x86_64"
  for d in \
    "${FLEET_LLVM_MINGW_DIR:-}" \
    "$extracted"; do
    if [[ -n "$d" && -f "$d/aarch64-w64-mingw32/bin/libunwind.dll" ]]; then
      printf '%s\n' "$d/aarch64-w64-mingw32/bin/libunwind.dll"
      return 0
    fi
  done
  src="$(find "$LLVM_MINGW_CACHE" -path '*/aarch64-w64-mingw32/bin/libunwind.dll' 2>/dev/null | head -n1 || true)"
  if [[ -n "$src" && -f "$src" ]]; then
    printf '%s\n' "$src"
    return 0
  fi
  if [[ -f "$LLVM_MINGW_CACHE/$LLVM_MINGW_TARBALL" ]]; then
    tar -xJf "$LLVM_MINGW_CACHE/$LLVM_MINGW_TARBALL" -C "$LLVM_MINGW_CACHE" \
      --wildcards '*/aarch64-w64-mingw32/bin/libunwind.dll'
    src="$(find "$LLVM_MINGW_CACHE" -path '*/aarch64-w64-mingw32/bin/libunwind.dll' 2>/dev/null | head -n1 || true)"
    if [[ -n "$src" && -f "$src" ]]; then
      printf '%s\n' "$src"
      return 0
    fi
  fi
  return 1
}

copy_libunwind_dll() {
  local out="$1"
  mkdir -p "$out"
  local src
  src="$(find_libunwind_dll)" || {
    echo "stage-worker-dist-docker: libunwind.dll not found in llvm-mingw aarch64 sysroot" >&2
    exit 1
  }
  cp "$src" "$out/libunwind.dll"
  chmod 644 "$out/libunwind.dll"
  echo "staged $out/libunwind.dll"
}

docker_cargo() {
  local image="$1"
  local target_dir="$2"
  shift 2
  mkdir -p "$CARGO_DOCKER" "$target_dir"
  # Host uid so target/ and dest/ stay user-owned.
  docker run --rm \
    --user "$(id -u):$(id -g)" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$target_dir:/target" \
    -w /src \
    "$image" \
    "$@"
}

stage_bookworm() {
  need_docker
  echo "building fleet-host fleet-worker fleet-driver (debug) in $BOOKWORM_IMAGE"
  docker_cargo "$BOOKWORM_IMAGE" "$BOOKWORM_TARGET" \
    cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver
  copy_bins "$BOOKWORM_TARGET/debug" "$DEST/linux-x86_64-bookworm"
  # Portable linux-x86_64 is the bookworm artifact (glibc 2.36), not Fedora.
  copy_bins "$BOOKWORM_TARGET/debug" "$DEST/linux-x86_64"
}

# rust:alpine has gcc but not musl-dev headers (ring/cc needs assert.h).
# apk requires root; dest bins are chowned back to the caller.
stage_musl_alpine() {
  echo "building fleet-host fleet-worker fleet-driver (debug) in $ALPINE_IMAGE + musl-dev"
  mkdir -p "$CARGO_DOCKER" "$MUSL_TARGET"
  if ! docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$MUSL_TARGET:/target" \
    -w /src \
    "$ALPINE_IMAGE" \
    sh -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apk add --no-cache musl-dev build-base || rc=$?
      if [ "$rc" -eq 0 ]; then
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target x86_64-unknown-linux-musl || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '; then
    echo "stage-worker-dist-docker: alpine musl cargo build failed" >&2
    return 1
  fi
  copy_bins "$MUSL_TARGET/x86_64-unknown-linux-musl/debug" "$DEST/linux-x86_64-musl"
}

# Official rust bookworm + musl-tools if alpine image/build fails.
stage_musl_bookworm_tools() {
  echo "building musl bins in $BOOKWORM_IMAGE + musl-tools"
  mkdir -p "$CARGO_DOCKER" "$MUSL_TARGET"
  docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$MUSL_TARGET:/target" \
    -w /src \
    "$BOOKWORM_IMAGE" \
    bash -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apt-get update -qq && apt-get install -y -qq musl-tools || rc=$?
      if [ "$rc" -eq 0 ]; then
        rustup target add x86_64-unknown-linux-musl || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target x86_64-unknown-linux-musl || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '
  copy_bins "$MUSL_TARGET/x86_64-unknown-linux-musl/debug" "$DEST/linux-x86_64-musl"
}

stage_musl() {
  need_docker
  # `if fn` disables set -e inside fn; alpine must return 1 (not exit) on cargo fail.
  if stage_musl_alpine; then
    return 0
  fi
  echo "stage-worker-dist-docker: alpine musl build failed; trying bookworm+musl-tools" >&2
  stage_musl_bookworm_tools
}

# Cross-compile glibc aarch64 on bookworm so Debian 12 / Ubuntu 22.04 ARM guests
# get the same portable max-GLIBC as linux-x86_64. Isolated target dir so this
# cannot collide with host cargo or the bookworm x86_64 tree.
stage_aarch64_gnu() {
  need_docker
  echo "building fleet-host fleet-worker fleet-driver (debug) for linux-aarch64 in $BOOKWORM_IMAGE + gcc-aarch64-linux-gnu"
  mkdir -p "$CARGO_DOCKER" "$AARCH64_GNU_TARGET"
  docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    -e CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    -e CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    -e AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$AARCH64_GNU_TARGET:/target" \
    -w /src \
    "$BOOKWORM_IMAGE" \
    bash -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apt-get update -qq && apt-get install -y -qq \
        gcc-aarch64-linux-gnu g++-aarch64-linux-gnu libc6-dev-arm64-cross || rc=$?
      if [ "$rc" -eq 0 ]; then
        rustup target add aarch64-unknown-linux-gnu || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target aarch64-unknown-linux-gnu || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '
  copy_bins "$AARCH64_GNU_TARGET/aarch64-unknown-linux-gnu/debug" "$DEST/linux-aarch64"
}

# Cross-compile glibc armv7hf on bookworm so Debian 12 / Ubuntu 22.04 32-bit
# ARM guests get the same portable max-GLIBC as linux-x86_64. Isolated target
# dir so this cannot collide with host cargo, bookworm x86_64, or aarch64-gnu.
# -latomic: 32-bit ARM needs it for AtomicU64 used by tokio/sqlx.
stage_armv7_gnu() {
  need_docker
  echo "building fleet-host fleet-worker fleet-driver (debug) for linux-armv7 in $BOOKWORM_IMAGE + gcc-arm-linux-gnueabihf"
  mkdir -p "$CARGO_DOCKER" "$ARMV7_GNU_TARGET"
  docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -e CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER=arm-linux-gnueabihf-gcc \
    -e CC_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-gcc \
    -e CXX_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-g++ \
    -e AR_armv7_unknown_linux_gnueabihf=arm-linux-gnueabihf-ar \
    -e CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_RUSTFLAGS="-C link-arg=-latomic" \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$ARMV7_GNU_TARGET:/target" \
    -w /src \
    "$BOOKWORM_IMAGE" \
    bash -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apt-get update -qq && apt-get install -y -qq \
        gcc-arm-linux-gnueabihf g++-arm-linux-gnueabihf libc6-dev-armhf-cross || rc=$?
      if [ "$rc" -eq 0 ]; then
        rustup target add armv7-unknown-linux-gnueabihf || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target armv7-unknown-linux-gnueabihf || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '
  copy_bins "$ARMV7_GNU_TARGET/armv7-unknown-linux-gnueabihf/debug" "$DEST/linux-armv7"
}

# rust:alpine + rustup aarch64-unknown-linux-musl. clang compiles ring/cc;
# rust-lld links rustc's self-contained musl (static-pie). Isolated target dir
# so this cannot collide with x86_64 musl or aarch64 gnu.
stage_aarch64_musl_alpine() {
  echo "building fleet-host fleet-worker fleet-driver (debug) for linux-aarch64-musl in $ALPINE_IMAGE"
  mkdir -p "$CARGO_DOCKER" "$AARCH64_MUSL_TARGET"
  if ! docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -e CC_aarch64_unknown_linux_musl=clang \
    -e AR_aarch64_unknown_linux_musl=llvm-ar \
    -e CFLAGS_aarch64_unknown_linux_musl="--target=aarch64-unknown-linux-musl" \
    -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$AARCH64_MUSL_TARGET:/target" \
    -w /src \
    "$ALPINE_IMAGE" \
    sh -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apk add --no-cache musl-dev build-base clang lld llvm || rc=$?
      if [ "$rc" -eq 0 ]; then
        rustup target add aarch64-unknown-linux-musl || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        host=$(rustc -vV | sed -n "s/^host: //p")
        sysroot=$(rustc --print sysroot)
        export PATH="${sysroot}/lib/rustlib/${host}/bin:${PATH}"
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target aarch64-unknown-linux-musl || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '; then
    echo "stage-worker-dist-docker: alpine aarch64-musl cargo build failed" >&2
    return 1
  fi
  copy_bins "$AARCH64_MUSL_TARGET/aarch64-unknown-linux-musl/debug" "$DEST/linux-aarch64-musl"
}

# rust:bookworm + musl.cc aarch64-linux-musl-cross if alpine fails.
stage_aarch64_musl_bookworm() {
  echo "building linux-aarch64-musl bins in $BOOKWORM_IMAGE + musl.cc aarch64-linux-musl-cross"
  mkdir -p "$CARGO_DOCKER" "$AARCH64_MUSL_TARGET"
  docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -e CC_aarch64_unknown_linux_musl=aarch64-linux-musl-gcc \
    -e CXX_aarch64_unknown_linux_musl=aarch64-linux-musl-g++ \
    -e AR_aarch64_unknown_linux_musl=aarch64-linux-musl-ar \
    -e CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=aarch64-linux-musl-gcc \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$AARCH64_MUSL_TARGET:/target" \
    -w /src \
    "$BOOKWORM_IMAGE" \
    bash -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apt-get update -qq && apt-get install -y -qq curl ca-certificates xz-utils || rc=$?
      if [ "$rc" -eq 0 ]; then
        mkdir -p /opt
        curl -fL --retry 3 -o /tmp/aarch64-linux-musl-cross.tgz \
          https://musl.cc/aarch64-linux-musl-cross.tgz || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        tar -xzf /tmp/aarch64-linux-musl-cross.tgz -C /opt || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        export PATH="/opt/aarch64-linux-musl-cross/bin:${PATH}"
        rustup target add aarch64-unknown-linux-musl || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target aarch64-unknown-linux-musl || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '
  copy_bins "$AARCH64_MUSL_TARGET/aarch64-unknown-linux-musl/debug" "$DEST/linux-aarch64-musl"
}

stage_aarch64_musl() {
  need_docker
  if stage_aarch64_musl_alpine; then
    return 0
  fi
  echo "stage-worker-dist-docker: alpine aarch64-musl build failed; trying bookworm+musl-cross" >&2
  stage_aarch64_musl_bookworm
}

# rust:alpine + rustup armv7-unknown-linux-musleabihf. clang compiles ring/cc;
# rust-lld links rustc's self-contained musl (static-pie). Isolated target dir
# so this cannot collide with x86_64 musl, aarch64 musl, or armv7 gnu.
# Do not pass -latomic here: alpine's libatomic is the host (x86_64). rustc
# emits ldrexd/strexd for this target. bookworm+musl.cc can add -latomic.
stage_armv7_musl_alpine() {
  echo "building fleet-host fleet-worker fleet-driver (debug) for linux-armv7-musl in $ALPINE_IMAGE"
  mkdir -p "$CARGO_DOCKER" "$ARMV7_MUSL_TARGET"
  if ! docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -e CC_armv7_unknown_linux_musleabihf=clang \
    -e AR_armv7_unknown_linux_musleabihf=llvm-ar \
    -e CFLAGS_armv7_unknown_linux_musleabihf="--target=armv7-unknown-linux-musleabihf" \
    -e CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=rust-lld \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$ARMV7_MUSL_TARGET:/target" \
    -w /src \
    "$ALPINE_IMAGE" \
    sh -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apk add --no-cache musl-dev build-base clang lld llvm || rc=$?
      if [ "$rc" -eq 0 ]; then
        rustup target add armv7-unknown-linux-musleabihf || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        host=$(rustc -vV | sed -n "s/^host: //p")
        sysroot=$(rustc --print sysroot)
        export PATH="${sysroot}/lib/rustlib/${host}/bin:${PATH}"
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target armv7-unknown-linux-musleabihf || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '; then
    echo "stage-worker-dist-docker: alpine armv7-musl cargo build failed" >&2
    return 1
  fi
  copy_bins "$ARMV7_MUSL_TARGET/armv7-unknown-linux-musleabihf/debug" "$DEST/linux-armv7-musl"
}

# rust:bookworm + musl.cc armv7l-linux-musleabihf-cross if alpine fails.
stage_armv7_musl_bookworm() {
  echo "building linux-armv7-musl bins in $BOOKWORM_IMAGE + musl.cc armv7l-linux-musleabihf-cross"
  mkdir -p "$CARGO_DOCKER" "$ARMV7_MUSL_TARGET"
  docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -e CC_armv7_unknown_linux_musleabihf=armv7l-linux-musleabihf-gcc \
    -e CXX_armv7_unknown_linux_musleabihf=armv7l-linux-musleabihf-g++ \
    -e AR_armv7_unknown_linux_musleabihf=armv7l-linux-musleabihf-ar \
    -e CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_LINKER=armv7l-linux-musleabihf-gcc \
    -e CARGO_TARGET_ARMV7_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS="-C link-arg=-latomic" \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$ARMV7_MUSL_TARGET:/target" \
    -w /src \
    "$BOOKWORM_IMAGE" \
    bash -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apt-get update -qq && apt-get install -y -qq curl ca-certificates xz-utils || rc=$?
      if [ "$rc" -eq 0 ]; then
        mkdir -p /opt
        curl -fL --retry 3 -o /tmp/armv7l-linux-musleabihf-cross.tgz \
          https://musl.cc/armv7l-linux-musleabihf-cross.tgz || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        tar -xzf /tmp/armv7l-linux-musleabihf-cross.tgz -C /opt || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        export PATH="/opt/armv7l-linux-musleabihf-cross/bin:${PATH}"
        rustup target add armv7-unknown-linux-musleabihf || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target armv7-unknown-linux-musleabihf || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '
  copy_bins "$ARMV7_MUSL_TARGET/armv7-unknown-linux-musleabihf/debug" "$DEST/linux-armv7-musl"
}

stage_armv7_musl() {
  need_docker
  if stage_armv7_musl_alpine; then
    return 0
  fi
  echo "stage-worker-dist-docker: alpine armv7-musl build failed; trying bookworm+musl-cross" >&2
  stage_armv7_musl_bookworm
}

# Cross-compile PE x86_64 via mingw-w64 so WS2022 / Windows 10+ guests can
# download fleet-host.exe. Isolated target dir so this cannot collide with
# host cargo or the bookworm linux trees. Fails closed on MSVC-only crates
# (do not invent a stub .exe).
stage_windows_gnu() {
  need_docker
  echo "building fleet-host fleet-worker fleet-driver (debug) for windows-x86_64 in $BOOKWORM_IMAGE + gcc-mingw-w64"
  mkdir -p "$CARGO_DOCKER" "$WINDOWS_GNU_TARGET"
  docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -e CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
    -e CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc \
    -e CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++ \
    -e AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$WINDOWS_GNU_TARGET:/target" \
    -w /src \
    "$BOOKWORM_IMAGE" \
    bash -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apt-get update -qq && apt-get install -y -qq \
        gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64 || rc=$?
      if [ "$rc" -eq 0 ]; then
        rustup target add x86_64-pc-windows-gnu || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target x86_64-pc-windows-gnu || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '
  copy_exe_bins "$WINDOWS_GNU_TARGET/x86_64-pc-windows-gnu/debug" "$DEST/windows-x86_64"
}

# Cross-compile PE aarch64 via llvm-mingw so WoA / Surface guests can download
# fleet-host.exe. rustc has aarch64-pc-windows-gnullvm, not *-gnu. Isolated
# target dir so this cannot collide with windows-x86_64 or linux trees.
# Fails closed if llvm-mingw cannot be fetched (do not invent a stub .exe).
stage_windows_arm64_gnullvm() {
  need_docker
  echo "building fleet-host fleet-worker fleet-driver (debug) for windows-arm64 in $BOOKWORM_IMAGE + llvm-mingw ${LLVM_MINGW_VER}"
  mkdir -p "$CARGO_DOCKER" "$WINDOWS_ARM64_TARGET" "$LLVM_MINGW_CACHE"
  local cache_tarball="$LLVM_MINGW_CACHE/$LLVM_MINGW_TARBALL"
  if [[ ! -f "$cache_tarball" ]]; then
    echo "stage-worker-dist-docker: fetching $LLVM_MINGW_URL"
    curl -fL --retry 3 -o "$cache_tarball" "$LLVM_MINGW_URL"
  fi
  docker run --rm \
    --user "0:0" \
    -e HOME=/tmp \
    -e CARGO_HOME=/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e HOST_UID="$(id -u)" \
    -e HOST_GID="$(id -g)" \
    -e CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_LINKER=aarch64-w64-mingw32-clang \
    -e CC_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang \
    -e CXX_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang++ \
    -e AR_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-ar \
    -e LLVM_MINGW_TARBALL="$LLVM_MINGW_TARBALL" \
    -v "$CARGO_DOCKER:/cargo" \
    -v "$ROOT:/src:rw" \
    -v "$WINDOWS_ARM64_TARGET:/target" \
    -v "$LLVM_MINGW_CACHE:/llvm-mingw-cache:ro" \
    -w /src \
    "$BOOKWORM_IMAGE" \
    bash -c '
      export PATH="/usr/local/cargo/bin:${PATH}"
      rc=0
      apt-get update -qq && apt-get install -y -qq xz-utils ca-certificates || rc=$?
      if [ "$rc" -eq 0 ]; then
        mkdir -p /opt
        tar -xJf "/llvm-mingw-cache/${LLVM_MINGW_TARBALL}" -C /opt || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        mingw_bin=$(find /opt -maxdepth 2 -type d -name bin | head -n1)
        export PATH="${mingw_bin}:${PATH}"
        rustup target add aarch64-pc-windows-gnullvm || rc=$?
      fi
      if [ "$rc" -eq 0 ]; then
        cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
          --target aarch64-pc-windows-gnullvm || rc=$?
      fi
      chown -R "${HOST_UID}:${HOST_GID}" /cargo /target || true
      exit "$rc"
    '
  copy_exe_bins "$WINDOWS_ARM64_TARGET/aarch64-pc-windows-gnullvm/debug" "$DEST/windows-arm64"
  copy_libunwind_dll "$DEST/windows-arm64"
}

TARGET="${1:-}"
case "$TARGET" in
  bookworm) stage_bookworm ;;
  musl) stage_musl ;;
  aarch64|linux-aarch64) stage_aarch64_gnu ;;
  armv7|linux-armv7) stage_armv7_gnu ;;
  aarch64-musl|linux-aarch64-musl) stage_aarch64_musl ;;
  armv7-musl|linux-armv7-musl) stage_armv7_musl ;;
  windows|windows-x86_64) stage_windows_gnu ;;
  windows-arm64) stage_windows_arm64_gnullvm ;;
  -h|--help|"")
    usage
    if [[ -z "$TARGET" ]]; then
      exit 1
    fi
    exit 0
    ;;
  *)
    echo "stage-worker-dist-docker: unknown target: $TARGET (want bookworm|musl|aarch64|armv7|aarch64-musl|armv7-musl|windows|windows-arm64)" >&2
    usage >&2
    exit 1
    ;;
esac
