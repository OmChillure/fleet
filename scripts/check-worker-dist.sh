#!/usr/bin/env bash
# Warn when a required OS does not have the verification fleet-host /
# fleet-driver versions from runner-packages.toml [verify]. Same rule as
# the Self-hosted page banner. Also warn when a package version is higher
# than [verify] (forgot to bump the checker).
# Not a hard fail — fleet-dev still starts.
# Update: scripts/stage-worker-dist.sh --all
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${FLEET_WORKER_DIST_DIR:-$ROOT/.tmp/vms/dist}"

# Keep in sync with REQUIRED_PACKAGE_PLATFORMS in fleet-runners-view.
PLATFORMS=(
  linux-x86_64
  linux-x86_64-musl
  linux-aarch64
  linux-aarch64-musl
  linux-armv7
  linux-armv7-musl
  windows-x86_64
  windows-arm64
)

toml_section_value() {
  local section="$1"
  local name="$2"
  local file="$ROOT/runner-packages.toml"
  python3 - "$file" "$section" "$name" <<'PY'
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

semver_ahead() {
  local package="$1"
  local verify="$2"
  python3 - "$package" "$verify" <<'PY'
import sys
def parts(v):
    out = []
    for p in v.split("."):
        digits = "".join(c for c in p if c.isdigit())
        out.append(int(digits) if digits else 0)
    return out
a, b = sys.argv[1], sys.argv[2]
if not a or not b:
    raise SystemExit(1)
pa, pb = parts(a), parts(b)
n = max(len(pa), len(pb))
pa += [0] * (n - len(pa))
pb += [0] * (n - len(pb))
raise SystemExit(0 if pa > pb else 1)
PY
}

PKG_HOST="$(toml_section_value versions fleet-host)"
PKG_DRIVER="$(toml_section_value versions fleet-driver)"
WANT_HOST="$(toml_section_value verify fleet-host)"
WANT_DRIVER="$(toml_section_value verify fleet-driver)"
if [[ -z "$WANT_HOST" || -z "$WANT_DRIVER" ]]; then
  echo "WARNING: runner-packages.toml is missing [verify] fleet-host / fleet-driver" >&2
  exit 0
fi
if [[ -n "$PKG_HOST" ]] && semver_ahead "$PKG_HOST" "$WANT_HOST"; then
  echo "WARNING: fleet-host package $PKG_HOST is higher than verification version $WANT_HOST" >&2
  echo "         bump [verify] in runner-packages.toml or the checker will flag every OS" >&2
fi
if [[ -n "$PKG_DRIVER" ]] && semver_ahead "$PKG_DRIVER" "$WANT_DRIVER"; then
  echo "WARNING: fleet-driver package $PKG_DRIVER is higher than verification version $WANT_DRIVER" >&2
  echo "         bump [verify] in runner-packages.toml or the checker will flag every OS" >&2
fi

if [[ ! -d "$DEST" ]]; then
  echo "WARNING: no staged worker dist at $DEST" >&2
  echo "         stage every OS: scripts/stage-worker-dist.sh --all" >&2
  exit 0
fi

stale=0
stale_plats=()
for plat in "${PLATFORMS[@]}"; do
  man="$DEST/$plat/manifest.json"
  if [[ ! -f "$man" ]]; then
    echo "WARNING: $plat has no staged package (want fleet-host $WANT_HOST / fleet-driver $WANT_DRIVER)" >&2
    stale=1
    stale_plats+=("$plat")
    continue
  fi
  read -r got_host got_driver < <(python3 - "$man" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
bins = m.get("bins") or {}
def ver(stem):
    for k, v in bins.items():
        name = k[:-4] if k.endswith(".exe") else k
        if name == stem:
            if isinstance(v, dict):
                return (v.get("version") or "").strip()
            return ""
    return ""
print(ver("fleet-host"), ver("fleet-driver"))
PY
)
  if [[ "$got_host" != "$WANT_HOST" || "$got_driver" != "$WANT_DRIVER" ]]; then
    if { [[ -n "$got_host" ]] && semver_ahead "$got_host" "$WANT_HOST"; } \
      || { [[ -n "$got_driver" ]] && semver_ahead "$got_driver" "$WANT_DRIVER"; }; then
      echo "WARNING: $plat package is higher than the verification version" >&2
      echo "         fleet-host ${got_host:-missing} (verify $WANT_HOST)" >&2
      echo "         fleet-driver ${got_driver:-missing} (verify $WANT_DRIVER)" >&2
      echo "         bump [verify] in runner-packages.toml" >&2
    else
      echo "WARNING: $plat is not current" >&2
      echo "         fleet-host ${got_host:-missing} (want $WANT_HOST)" >&2
      echo "         fleet-driver ${got_driver:-missing} (want $WANT_DRIVER)" >&2
    fi
    stale=1
    stale_plats+=("$plat")
  fi
done

if [[ "$stale" -eq 1 ]]; then
  echo "WARNING: OS builds not on fleet-host $WANT_HOST / fleet-driver $WANT_DRIVER: ${stale_plats[*]}" >&2
  echo "         update: scripts/stage-worker-dist.sh --all" >&2
fi
exit 0
