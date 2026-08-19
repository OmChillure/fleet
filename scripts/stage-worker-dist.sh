#!/usr/bin/env bash
# Build fleet-host / fleet-worker / fleet-driver and copy them into
# $FLEET_WORKER_DIST_DIR/$PLATFORM (default .tmp/vms/dist/$PLATFORM) so
# GET /install/worker/bin/{platform}/{bin} can serve them.
#
# Usage:
#   scripts/stage-worker-dist.sh              # native host toolchain (default)
#   scripts/stage-worker-dist.sh --native     # same
#   scripts/stage-worker-dist.sh --bookworm   # docker rust:1.88-bookworm → linux-x86_64
#   scripts/stage-worker-dist.sh --aarch64    # docker bookworm + aarch64-gnu → linux-aarch64
#   scripts/stage-worker-dist.sh --armv7      # docker bookworm + armv7-gnu → linux-armv7
#   scripts/stage-worker-dist.sh --musl       # musl → linux-x86_64-musl
#   scripts/stage-worker-dist.sh --aarch64-musl # musl → linux-aarch64-musl
#   scripts/stage-worker-dist.sh --armv7-musl # musl → linux-armv7-musl
#   scripts/stage-worker-dist.sh --windows    # mingw-w64 gnu → windows-x86_64 .exe
#   scripts/stage-worker-dist.sh --windows-arm64 # llvm-mingw gnullvm → windows-arm64 .exe
#   scripts/stage-worker-dist.sh --portable   # bookworm + musl (x86_64)
#
# --bookworm overwrites linux-x86_64 with the Debian 12 (glibc 2.36) artifact
# so Debian 12 / Ubuntu 22.04 guests can run it. Native Fedora bins need 2.39+.
# --aarch64 cross-compiles the same glibc 2.36 artifact for linux-aarch64.
# --armv7 cross-compiles the same glibc 2.36 artifact for linux-armv7 (32-bit).
# --aarch64-musl cross-compiles static musl for Alpine ARM (linux-aarch64-musl).
# --armv7-musl cross-compiles static musl for Alpine armhf (linux-armv7-musl).
# --windows cross-compiles x86_64-pc-windows-gnu (.exe) with mingw-w64 (host
# gcc or docker bookworm + gcc-mingw-w64). Isolated target/docker-windows-gnu.
# --windows-arm64 cross-compiles aarch64-pc-windows-gnullvm (.exe) with
# llvm-mingw (host clang or docker bookworm + llvm-mingw). Isolated
# target/docker-windows-arm64-gnu. No rustc aarch64-pc-windows-gnu target.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DEST="${FLEET_WORKER_DIST_DIR:-.tmp/vms/dist}"
DOCKER_HELPER="$ROOT/scripts/stage-worker-dist-docker.sh"
MODE=native

usage() {
  sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --native) MODE=native ;;
    --bookworm) MODE=bookworm ;;
    --aarch64|--linux-aarch64) MODE=aarch64 ;;
    --armv7|--linux-armv7) MODE=armv7 ;;
    --aarch64-musl|--linux-aarch64-musl) MODE=aarch64-musl ;;
    --armv7-musl|--linux-armv7-musl) MODE=armv7-musl ;;
    --windows|--windows-x86_64) MODE=windows ;;
    --windows-arm64) MODE=windows-arm64 ;;
    --musl) MODE=musl ;;
    --portable|--all) MODE=portable ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "stage-worker-dist: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
  shift
done

copy_bins() {
  local src="$1"
  local out="$2"
  mkdir -p "$out"
  local bin
  for bin in fleet-host fleet-worker fleet-driver; do
    if [[ ! -f "$src/$bin" ]]; then
      echo "stage-worker-dist: missing $src/$bin after cargo build" >&2
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
      echo "stage-worker-dist: missing $src/$bin after cargo build" >&2
      exit 1
    fi
    cp "$src/$bin" "$out/$bin"
    chmod 755 "$out/$bin"
    echo "staged $out/$bin"
  done
}

stage_native() {
  local platform
  platform="$(FLEET_DETECT_ONLY=1 sh "$ROOT/scripts/install-worker/install-worker.sh")"
  if [[ -z "$platform" ]]; then
    echo "stage-worker-dist: failed to detect platform" >&2
    exit 1
  fi

  echo "building fleet-host fleet-worker fleet-driver (debug) for $platform"
  cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver

  local target_dir="${CARGO_TARGET_DIR:-$ROOT/target}"
  copy_bins "$target_dir/debug" "$DEST/$platform"
}

# Host rustup musl target. Needs musl-gcc for ring/cc. Falls back to docker
# (alpine + musl-dev, or bookworm + musl-tools) when the host cannot link musl.
stage_musl_host() {
  if ! command -v rustup >/dev/null 2>&1; then
    echo "stage-worker-dist: rustup not on PATH; using docker for musl" >&2
    return 1
  fi
  if ! command -v musl-gcc >/dev/null 2>&1 \
      && ! command -v x86_64-linux-musl-gcc >/dev/null 2>&1; then
    echo "stage-worker-dist: no musl-gcc on PATH; using docker for musl" >&2
    return 1
  fi
  if ! rustup target list --installed | grep -qx 'x86_64-unknown-linux-musl'; then
    echo "stage-worker-dist: rustup target add x86_64-unknown-linux-musl"
    rustup target add x86_64-unknown-linux-musl || return 1
  fi

  echo "building fleet-host fleet-worker fleet-driver (debug) for linux-x86_64-musl"
  if ! cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
      --target x86_64-unknown-linux-musl; then
    echo "stage-worker-dist: host musl cargo build failed; will try docker" >&2
    return 1
  fi

  local target_dir="${CARGO_TARGET_DIR:-$ROOT/target}"
  copy_bins "$target_dir/x86_64-unknown-linux-musl/debug" "$DEST/linux-x86_64-musl"
}

stage_musl() {
  if stage_musl_host; then
    return 0
  fi
  if [[ ! -x "$DOCKER_HELPER" ]]; then
    echo "stage-worker-dist: musl host build failed and $DOCKER_HELPER is missing" >&2
    exit 1
  fi
  "$DOCKER_HELPER" musl
}

stage_bookworm() {
  if [[ ! -x "$DOCKER_HELPER" ]]; then
    echo "stage-worker-dist: $DOCKER_HELPER is missing (needed for --bookworm)" >&2
    exit 1
  fi
  "$DOCKER_HELPER" bookworm
}

stage_aarch64() {
  if [[ ! -x "$DOCKER_HELPER" ]]; then
    echo "stage-worker-dist: $DOCKER_HELPER is missing (needed for --aarch64)" >&2
    exit 1
  fi
  "$DOCKER_HELPER" aarch64
}

stage_armv7() {
  if [[ ! -x "$DOCKER_HELPER" ]]; then
    echo "stage-worker-dist: $DOCKER_HELPER is missing (needed for --armv7)" >&2
    exit 1
  fi
  "$DOCKER_HELPER" armv7
}

stage_aarch64_musl() {
  if [[ ! -x "$DOCKER_HELPER" ]]; then
    echo "stage-worker-dist: $DOCKER_HELPER is missing (needed for --aarch64-musl)" >&2
    exit 1
  fi
  "$DOCKER_HELPER" aarch64-musl
}

stage_armv7_musl() {
  if [[ ! -x "$DOCKER_HELPER" ]]; then
    echo "stage-worker-dist: $DOCKER_HELPER is missing (needed for --armv7-musl)" >&2
    exit 1
  fi
  "$DOCKER_HELPER" armv7-musl
}

# Host rustup windows-gnu + mingw-w64 gcc. Falls back to docker
# (bookworm + gcc-mingw-w64) when the host cannot link PE binaries.
stage_windows_host() {
  if ! command -v rustup >/dev/null 2>&1; then
    echo "stage-worker-dist: rustup not on PATH; using docker for windows-gnu" >&2
    return 1
  fi
  if ! command -v x86_64-w64-mingw32-gcc >/dev/null 2>&1; then
    echo "stage-worker-dist: no x86_64-w64-mingw32-gcc on PATH; using docker for windows-gnu" >&2
    return 1
  fi
  if ! rustup target list --installed | grep -qx 'x86_64-pc-windows-gnu'; then
    echo "stage-worker-dist: rustup target add x86_64-pc-windows-gnu"
    rustup target add x86_64-pc-windows-gnu || return 1
  fi

  local target_dir="${FLEET_WINDOWS_GNU_TARGET_DIR:-$ROOT/target/docker-windows-gnu}"
  mkdir -p "$target_dir"
  echo "building fleet-host fleet-worker fleet-driver (debug) for windows-x86_64"
  if ! CARGO_TARGET_DIR="$target_dir" \
      CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
      CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc \
      CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++ \
      AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar \
      cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
        --target x86_64-pc-windows-gnu; then
    echo "stage-worker-dist: host windows-gnu cargo build failed; will try docker" >&2
    return 1
  fi

  copy_exe_bins "$target_dir/x86_64-pc-windows-gnu/debug" "$DEST/windows-x86_64"
}

stage_windows() {
  if stage_windows_host; then
    return 0
  fi
  if [[ ! -x "$DOCKER_HELPER" ]]; then
    echo "stage-worker-dist: windows-gnu host build failed and $DOCKER_HELPER is missing" >&2
    exit 1
  fi
  "$DOCKER_HELPER" windows
}

# llvm-mingw aarch64-w64-mingw32-clang on PATH or under .tmp/llvm-mingw.
# rustc has aarch64-pc-windows-gnullvm (not *-gnu). Falls back to docker.
# gnullvm PE imports libunwind.dll; stage it next to the three .exe files.
find_libunwind_dll() {
  local clang_dir src d
  if command -v aarch64-w64-mingw32-clang >/dev/null 2>&1; then
    clang_dir="$(dirname "$(command -v aarch64-w64-mingw32-clang)")"
    if [[ -f "$clang_dir/../aarch64-w64-mingw32/bin/libunwind.dll" ]]; then
      src="$(cd "$clang_dir/../aarch64-w64-mingw32/bin" && pwd)/libunwind.dll"
      printf '%s\n' "$src"
      return 0
    fi
  fi
  for d in \
    "${FLEET_LLVM_MINGW_DIR:-}" \
    "$ROOT/.tmp/llvm-mingw/llvm-mingw-20260616-ucrt-ubuntu-22.04-x86_64"; do
    if [[ -n "$d" && -f "$d/aarch64-w64-mingw32/bin/libunwind.dll" ]]; then
      printf '%s\n' "$d/aarch64-w64-mingw32/bin/libunwind.dll"
      return 0
    fi
  done
  src="$(find "$ROOT/.tmp/llvm-mingw" -path '*/aarch64-w64-mingw32/bin/libunwind.dll' 2>/dev/null | head -n1 || true)"
  if [[ -n "$src" && -f "$src" ]]; then
    printf '%s\n' "$src"
    return 0
  fi
  return 1
}

copy_libunwind_dll() {
  local out="$1"
  mkdir -p "$out"
  local src
  src="$(find_libunwind_dll)" || {
    echo "stage-worker-dist: libunwind.dll not found in llvm-mingw aarch64 sysroot" >&2
    exit 1
  }
  cp "$src" "$out/libunwind.dll"
  chmod 644 "$out/libunwind.dll"
  echo "staged $out/libunwind.dll"
}

find_llvm_mingw_bin() {
  if command -v aarch64-w64-mingw32-clang >/dev/null 2>&1; then
    return 0
  fi
  local d
  for d in \
    "${FLEET_LLVM_MINGW_DIR:-}" \
    "$ROOT/.tmp/llvm-mingw/llvm-mingw-20260616-ucrt-ubuntu-22.04-x86_64"; do
    if [[ -n "$d" && -x "$d/bin/aarch64-w64-mingw32-clang" ]]; then
      PATH="$d/bin:$PATH"
      export PATH
      return 0
    fi
  done
  local hit
  hit="$(find "$ROOT/.tmp/llvm-mingw" -maxdepth 2 -type f -name aarch64-w64-mingw32-clang 2>/dev/null | head -n1 || true)"
  if [[ -n "$hit" && -x "$hit" ]]; then
    PATH="$(dirname "$hit"):$PATH"
    export PATH
    return 0
  fi
  return 1
}

stage_windows_arm64_host() {
  if ! command -v rustup >/dev/null 2>&1; then
    echo "stage-worker-dist: rustup not on PATH; using docker for windows-arm64" >&2
    return 1
  fi
  if ! find_llvm_mingw_bin; then
    echo "stage-worker-dist: no aarch64-w64-mingw32-clang on PATH; using docker for windows-arm64" >&2
    return 1
  fi
  if ! rustup target list --installed | grep -qx 'aarch64-pc-windows-gnullvm'; then
    echo "stage-worker-dist: rustup target add aarch64-pc-windows-gnullvm"
    rustup target add aarch64-pc-windows-gnullvm || return 1
  fi

  local target_dir="${FLEET_WINDOWS_ARM64_TARGET_DIR:-$ROOT/target/docker-windows-arm64-gnu}"
  mkdir -p "$target_dir"
  echo "building fleet-host fleet-worker fleet-driver (debug) for windows-arm64"
  if ! CARGO_TARGET_DIR="$target_dir" \
      CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_LINKER=aarch64-w64-mingw32-clang \
      CC_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang \
      CXX_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang++ \
      AR_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-ar \
      cargo build -p fleet-host -p fleet-orchestrator -p fleet-driver \
        --target aarch64-pc-windows-gnullvm; then
    echo "stage-worker-dist: host windows-arm64 cargo build failed; will try docker" >&2
    return 1
  fi

  copy_exe_bins "$target_dir/aarch64-pc-windows-gnullvm/debug" "$DEST/windows-arm64"
  copy_libunwind_dll "$DEST/windows-arm64"
}

stage_windows_arm64() {
  if stage_windows_arm64_host; then
    return 0
  fi
  if [[ ! -x "$DOCKER_HELPER" ]]; then
    echo "stage-worker-dist: windows-arm64 host build failed and $DOCKER_HELPER is missing" >&2
    exit 1
  fi
  "$DOCKER_HELPER" windows-arm64
}

case "$MODE" in
  native) stage_native ;;
  bookworm) stage_bookworm ;;
  aarch64) stage_aarch64 ;;
  armv7) stage_armv7 ;;
  aarch64-musl) stage_aarch64_musl ;;
  armv7-musl) stage_armv7_musl ;;
  windows) stage_windows ;;
  windows-arm64) stage_windows_arm64 ;;
  musl) stage_musl ;;
  portable)
    stage_bookworm
    stage_musl
    ;;
  *)
    echo "stage-worker-dist: internal error: mode=$MODE" >&2
    exit 1
    ;;
esac
