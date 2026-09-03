#!/usr/bin/env bash
# Build fleet-host / fleet-driver and copy them into
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
#   scripts/stage-worker-dist.sh --all        # every required OS
#   scripts/stage-worker-dist.sh --write-manifest [dir]
#       write manifest.json next to already-staged bins (no rebuild)
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
export FLEET_GIT_SHA="${FLEET_GIT_SHA:-$(bash "$ROOT/scripts/tree-id.sh")}"

DEST="${FLEET_WORKER_DIST_DIR:-.tmp/vms/dist}"
DOCKER_HELPER="$ROOT/scripts/stage-worker-dist-docker.sh"
MODE=native

usage() {
  sed -n '2,32p' "$0" | sed 's/^# \{0,1\}//'
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
    --portable) MODE=portable ;;
    --all) MODE=all ;;
    --write-manifest)
      MODE=write-manifest
      if [[ $# -gt 1 && "${2:-}" != -* ]]; then
        MANIFEST_DIR="$2"
        shift
      fi
      ;;
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

file_sha256() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    echo "stage-worker-dist: need sha256sum or shasum" >&2
    exit 1
  fi
}

write_sha256() {
  local file="$1"
  file_sha256 "$file" >"$file.sha256"
}

json_quote() {
  printf '"%s"' "${1//\"/\\\"}"
}

# File hash is not bake id. 64-hex is sha256 of a bin; tree-id is 40 hex
# (optionally -<8-12 hex> when dirty).
is_file_sha256() {
  [[ "$1" =~ ^[0-9a-fA-F]{64}$ ]]
}

pkg_version_from_toml() {
  local name="$1"
  local file="$ROOT/runner-packages.toml"
  python3 - "$file" versions "$name" <<'PY'
import re, sys
text = open(sys.argv[1], encoding="utf-8").read()
section, name = sys.argv[2], sys.argv[3]
header = f"[{section}]"
in_section = False
for line in text.splitlines():
    line = line.strip()
    if line.startswith("["):
        in_section = line == header
        continue
    if not in_section:
        continue
    m = re.match(rf"{re.escape(name)}\s*=\s*\"([^\"]+)\"", line)
    if m:
        sys.stdout.write(m.group(1))
        break
PY
}

# Bake recorded in fleet-host (option_env FLEET_GIT_SHA), immediately before
# the git_sha symbol. Do not invent a tree-id for an unbaked PE.
baked_sha_from_host() {
  local bin="$1"
  python3 - "$bin" <<'PY'
import os, re, sys
data = open(sys.argv[1], "rb").read()
# Dirty tree-id (40 hex + suffix) is distinctive. Clean 40-hex alone is not
# (lots of rustc hashes). Debug layout: "<sha>git_sha". Release: "<sha>crates/".
for pat in (
    rb"([0-9a-f]{40}-[0-9a-f]{8,12})(?:git_sha|crates/)",
    rb"([0-9a-f]{40})git_sha",
    rb"([0-9a-f]{40}-[0-9a-f]{8,12})(?![0-9a-f])",
):
    m = re.search(pat, data)
    if m:
        sys.stdout.write(m.group(1).decode("ascii"))
        raise SystemExit
want = os.environ.get("FLEET_GIT_SHA", "").encode()
if want and want in data:
    sys.stdout.write(want.decode())
PY
}

# Write $out/manifest.json next to the fleet-host / fleet-driver bins.
# Fail closed: missing platform or missing host/driver hash must not be
# reported current. File hash is not bake id (git_sha).
# git_sha is the bake inside fleet-host. --write-manifest must not stamp
# today's tree-id onto unchanged / unbaked bins (that made x1 look current).
# After copy_bins, set FLEET_MANIFEST_STAMP=1 to record $FLEET_GIT_SHA
# when the binary has no extractable bake.
write_manifest() {
  local out="$1"
  local platform git_sha baked hostbin
  platform="$(basename "$out")"

  local names=()
  if [[ -f "$out/fleet-host.exe" ]]; then
    names=(fleet-host.exe fleet-driver.exe)
    hostbin="$out/fleet-host.exe"
  elif [[ -f "$out/fleet-host" ]]; then
    names=(fleet-host fleet-driver)
    hostbin="$out/fleet-host"
  else
    echo "stage-worker-dist: write_manifest: no fleet-host in $out" >&2
    return 1
  fi

  local bin
  for bin in "${names[@]}"; do
    if [[ ! -f "$out/$bin" ]]; then
      echo "stage-worker-dist: write_manifest: missing $out/$bin" >&2
      return 1
    fi
  done

  local -a bin_shas=()
  local sha
  for bin in "${names[@]}"; do
    sha="$(file_sha256 "$out/$bin")"
    if [[ -z "$sha" ]]; then
      echo "stage-worker-dist: write_manifest: empty sha256 for $out/$bin" >&2
      return 1
    fi
    bin_shas+=("$sha")
  done

  baked="$(baked_sha_from_host "$hostbin" || true)"
  if [[ -n "$baked" ]]; then
    git_sha="$baked"
  elif [[ "${FLEET_MANIFEST_STAMP:-}" = 1 ]]; then
    git_sha="${FLEET_GIT_SHA:-}"
    if [[ -z "$git_sha" ]]; then
      git_sha="$(bash "$ROOT/scripts/tree-id.sh")"
    fi
  else
    git_sha=""
  fi

  if [[ -n "$git_sha" ]]; then
    if is_file_sha256 "$git_sha"; then
      echo "stage-worker-dist: write_manifest: git_sha must not be a file hash" >&2
      return 1
    fi
    local bin_sha
    for bin_sha in "${bin_shas[@]}"; do
      if [[ "${git_sha,,}" == "${bin_sha,,}" ]]; then
        echo "stage-worker-dist: write_manifest: git_sha must not be a file hash" >&2
        return 1
      fi
    done
  fi

  local staged_at
  staged_at="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  local host_ver driver_ver
  host_ver="$(pkg_version_from_toml fleet-host)"
  driver_ver="$(pkg_version_from_toml fleet-driver)"

  local tmp="$out/manifest.json.tmp"
  {
    printf '{\n  "git_sha": %s,\n  "platform": %s,\n  "staged_at": %s,\n  "bins": {\n' \
      "$(json_quote "$git_sha")" "$(json_quote "$platform")" "$(json_quote "$staged_at")"
    local i=0
    local n=${#names[@]}
    local size ver stem
    for bin in "${names[@]}"; do
      sha="${bin_shas[$i]}"
      size="$(wc -c <"$out/$bin" | tr -d '[:space:]')"
      stem="${bin%.exe}"
      if [[ "$stem" == "fleet-driver" ]]; then
        ver="$driver_ver"
      else
        ver="$host_ver"
      fi
      printf '    %s: { "sha256": %s, "size": %s, "version": %s }' \
        "$(json_quote "$bin")" "$(json_quote "$sha")" "$size" "$(json_quote "$ver")"
      i=$((i + 1))
      if [[ "$i" -lt "$n" ]]; then
        printf ',\n'
      else
        printf '\n'
      fi
    done
    printf '  }\n}\n'
  } >"$tmp" || {
    rm -f "$tmp"
    echo "stage-worker-dist: write_manifest: failed writing $tmp" >&2
    return 1
  }
  mv -f "$tmp" "$out/manifest.json"
  echo "staged $out/manifest.json"
}

write_manifests_in() {
  local root="${1:-$DEST}"
  if [[ -f "$root/fleet-host" || -f "$root/fleet-host.exe" ]]; then
    write_manifest "$root"
    return
  fi
  local d
  local any=0
  local has_host has_driver
  for d in "$root"/*/; do
    [[ -d "$d" ]] || continue
    has_host=0
    has_driver=0
    if [[ -f "${d}fleet-host" || -f "${d}fleet-host.exe" ]]; then
      has_host=1
    fi
    if [[ -f "${d}fleet-driver" || -f "${d}fleet-driver.exe" ]]; then
      has_driver=1
    fi
    if [[ "$has_host" -eq 0 && "$has_driver" -eq 0 ]]; then
      continue
    fi
    # Incomplete platform: do not write a current-looking manifest.
    if [[ "$has_host" -eq 0 || "$has_driver" -eq 0 ]]; then
      echo "stage-worker-dist: write_manifest: missing host/driver in ${d%/}" >&2
      exit 1
    fi
    write_manifest "${d%/}"
    any=1
  done
  if [[ "$any" -eq 0 ]]; then
    echo "stage-worker-dist: no staged platforms under $root" >&2
    exit 1
  fi
}

copy_bins() {
  local src="$1"
  local out="$2"
  mkdir -p "$out"
  local bin
  for bin in fleet-host fleet-driver; do
    if [[ ! -f "$src/$bin" ]]; then
      echo "stage-worker-dist: missing $src/$bin after cargo build" >&2
      exit 1
    fi
    cp "$src/$bin" "$out/$bin"
    chmod 755 "$out/$bin"
    write_sha256 "$out/$bin"
    echo "staged $out/$bin"
  done
  FLEET_MANIFEST_STAMP=1 write_manifest "$out"
}

copy_exe_bins() {
  local src="$1"
  local out="$2"
  mkdir -p "$out"
  local bin
  for bin in fleet-host.exe fleet-driver.exe; do
    if [[ ! -f "$src/$bin" ]]; then
      echo "stage-worker-dist: missing $src/$bin after cargo build" >&2
      exit 1
    fi
    cp "$src/$bin" "$out/$bin"
    chmod 755 "$out/$bin"
    write_sha256 "$out/$bin"
    echo "staged $out/$bin"
  done
  FLEET_MANIFEST_STAMP=1 write_manifest "$out"
}

stage_native() {
  local platform
  platform="$(FLEET_DETECT_ONLY=1 sh "$ROOT/scripts/install-worker/install-worker.sh")"
  if [[ -z "$platform" ]]; then
    echo "stage-worker-dist: failed to detect platform" >&2
    exit 1
  fi

  echo "building fleet-host fleet-driver (debug) for $platform"
  cargo build -p fleet-host -p fleet-driver

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

  echo "building fleet-host fleet-driver (debug) for linux-x86_64-musl"
  if ! cargo build -p fleet-host -p fleet-driver \
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
  echo "building fleet-host fleet-driver (release) for windows-x86_64"
  if ! CARGO_TARGET_DIR="$target_dir" \
      CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc \
      CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc \
      CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++ \
      AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar \
      cargo build --release -p fleet-host -p fleet-driver \
        --target x86_64-pc-windows-gnu; then
    echo "stage-worker-dist: host windows-gnu cargo build failed; will try docker" >&2
    return 1
  fi

  copy_exe_bins "$target_dir/x86_64-pc-windows-gnu/release" "$DEST/windows-x86_64"
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
  echo "building fleet-host fleet-driver (debug) for windows-arm64"
  if ! CARGO_TARGET_DIR="$target_dir" \
      CARGO_TARGET_AARCH64_PC_WINDOWS_GNULLVM_LINKER=aarch64-w64-mingw32-clang \
      CC_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang \
      CXX_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-clang++ \
      AR_aarch64_pc_windows_gnullvm=aarch64-w64-mingw32-ar \
      cargo build -p fleet-host -p fleet-driver \
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
  write-manifest) write_manifests_in "${MANIFEST_DIR:-$DEST}" ;;
  portable)
    stage_bookworm
    stage_musl
    ;;
  all)
    stage_bookworm
    stage_musl
    stage_aarch64
    stage_armv7
    stage_aarch64_musl
    stage_armv7_musl
    stage_windows
    stage_windows_arm64
    ;;
  *)
    echo "stage-worker-dist: internal error: mode=$MODE" >&2
    exit 1
    ;;
esac
