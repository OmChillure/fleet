#!/bin/sh
# Shared Unix installer. Sourced by scripts/install-worker/<platform>.sh
# Expects: FLEET_PLATFORM, FLEET_URL (or FLEET_INSTALL_ORIGIN), optional FLEET_PAIR_TOKEN
# Optional: FLEET_WORKER_BUILD_FROM_SOURCE=1, FLEET_PREFIX, FLEET_REPO
set -eu

ORIGIN="${FLEET_INSTALL_ORIGIN:-${FLEET_URL:-}}"
PLATFORM="${FLEET_PLATFORM:-}"
PREFIX="${FLEET_PREFIX:-}"
REPO="${FLEET_REPO:-}"

info() { printf '%s\n' "$*"; }
err() { printf 'fleet-host install: %s\n' "$*" >&2; }
die() { err "$*"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

[ -n "$ORIGIN" ] || die "FLEET_URL is not set"
[ -n "$PLATFORM" ] || die "FLEET_PLATFORM is not set"

if [ -z "$PREFIX" ]; then
    if [ "$(id -u)" -eq 0 ]; then
        PREFIX=/usr/local
    else
        PREFIX="${HOME}/.local"
    fi
fi
BIN_DIR="${PREFIX}/bin"
mkdir -p "$BIN_DIR"

fetch_to() {
    # dest is used by the caller; must not clobber it here.
    _url=$1
    _dest=$2
    if have curl; then
        curl -fL --progress-bar "$_url" -o "$_dest"
    elif have wget; then
        wget -O "$_dest" "$_url"
    elif have fetch; then
        # FreeBSD: fetch -o dest url (not dest= — keep dest-leak fix)
        fetch -o "$_dest" "$_url"
    else
        die "need curl, wget, or fetch"
    fi
}

file_sha256() {
    if have sha256sum; then
        sha256sum "$1" | awk '{print $1}'
    elif have shasum; then
        shasum -a 256 "$1" | awk '{print $1}'
    elif have sha256; then
        sha256 -q "$1"
    else
        die "need sha256sum, shasum, or sha256"
    fi
}

norm_sha() {
    printf '%s\n' "$1" | tr 'A-F' 'a-f'
}

install_bins_from_cp() {
    base="$ORIGIN/install/worker/bin/$PLATFORM"
    ok=0
    for bin in fleet-host fleet-worker fleet-driver; do
        dest="$BIN_DIR/$bin"
        want=""
        if fetch_to "$base/$bin.sha256" "$dest.sha256.tmp" 2>/dev/null; then
            want=$(norm_sha "$(awk '{print $1}' "$dest.sha256.tmp")")
            rm -f "$dest.sha256.tmp"
        else
            rm -f "$dest.sha256.tmp"
        fi
        if [ -n "$want" ] && [ -f "$dest" ]; then
            got=$(norm_sha "$(file_sha256 "$dest")")
            if [ "$got" = "$want" ]; then
                info "unchanged $dest"
                ok=1
                continue
            fi
        fi
        if fetch_to "$base/$bin" "$dest.tmp" 2>/dev/null; then
            if [ -n "$want" ]; then
                got=$(norm_sha "$(file_sha256 "$dest.tmp")")
                if [ "$got" != "$want" ]; then
                    rm -f "$dest.tmp"
                    die "checksum mismatch for $bin"
                fi
            fi
            chmod 755 "$dest.tmp"
            mv "$dest.tmp" "$dest"
            info "installed $dest"
            ok=1
        else
            rm -f "$dest.tmp"
            err "no prebuilt $bin at $base/$bin"
        fi
    done
    [ "$ok" = 1 ] || return 1
    have_host=0
    [ -x "$BIN_DIR/fleet-host" ] && have_host=1
    [ "$have_host" = 1 ] || return 1
    return 0
}

install_bins_from_source() {
    [ "${FLEET_WORKER_BUILD_FROM_SOURCE:-}" = 1 ] || return 1
    have cargo || die "FLEET_WORKER_BUILD_FROM_SOURCE=1 but cargo is not on PATH"
    src=$REPO
    if [ -z "$src" ]; then
        if [ -f "$PWD/Cargo.toml" ] && grep -q 'name = "fleet-server"' "$PWD/crates/fleet-server/Cargo.toml" 2>/dev/null; then
            src=$PWD
        fi
    fi
    [ -n "$src" ] || die "set FLEET_REPO to the agent-fleet checkout to build from source"
    info "building fleet-host + fleet-worker + fleet-driver from $src"
    (cd "$src" && cargo build --release -p fleet-host -p fleet-orchestrator -p fleet-driver)
    cp "$src/target/release/fleet-host" "$BIN_DIR/fleet-host"
    if [ -f "$src/target/release/fleet-worker" ]; then
        cp "$src/target/release/fleet-worker" "$BIN_DIR/fleet-worker"
    fi
    if [ -f "$src/target/release/fleet-driver" ]; then
        cp "$src/target/release/fleet-driver" "$BIN_DIR/fleet-driver"
    fi
    chmod 755 "$BIN_DIR/fleet-host"
    info "installed $BIN_DIR/fleet-host (from source)"
}

install_service_systemd() {
    unit_dir=""
    if [ "$(id -u)" -eq 0 ]; then
        unit_dir=/etc/systemd/system
        unit=fleet-host.service
    else
        unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
        unit=fleet-host.service
        mkdir -p "$unit_dir"
    fi
    cat >"$unit_dir/$unit" <<EOF
[Unit]
Description=Agent Fleet worker host
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=${BIN_DIR}/fleet-host run
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
EOF
    if [ "$(id -u)" -eq 0 ]; then
        systemctl daemon-reload 2>/dev/null || true
        info "systemd unit: $unit_dir/$unit"
    else
        systemctl --user daemon-reload 2>/dev/null || true
        info "systemd user unit: $unit_dir/$unit"
    fi
}

start_host_service() {
    case ${PLATFORM:-} in
        linux-*)
            if have systemctl; then
                if [ "$(id -u)" -eq 0 ]; then
                    systemctl enable --now fleet-host
                    info "started systemd unit fleet-host (systemctl status fleet-host)"
                    return 0
                fi
                systemctl --user enable --now fleet-host
                info "started systemd user unit fleet-host (systemctl --user status fleet-host)"
                return 0
            fi
            info "no systemd; keep this running: $BIN_DIR/fleet-host run"
            return 1
            ;;
        darwin-*)
            plist="${HOME}/Library/LaunchAgents/ai.agentfleet.host.plist"
            uid=$(id -u)
            if have launchctl && [ -f "$plist" ]; then
                launchctl bootout "gui/${uid}" "$plist" 2>/dev/null || true
                if launchctl bootstrap "gui/${uid}" "$plist" 2>/dev/null \
                    || launchctl load "$plist" 2>/dev/null; then
                    info "started launchd $plist"
                    return 0
                fi
            fi
            info "start: launchctl load $plist"
            info "  (or: $BIN_DIR/fleet-host run)"
            return 1
            ;;
        freebsd-*)
            if [ "$(id -u)" -eq 0 ] && [ -x /usr/local/etc/rc.d/fleet-host ]; then
                sysrc fleet_host_enable=YES 2>/dev/null || true
                /usr/local/etc/rc.d/fleet-host start
                info "started rc.d fleet-host"
                return 0
            fi
            info "start: $BIN_DIR/fleet-host run"
            info "  (root: sysrc fleet_host_enable=YES && service fleet_host start)"
            return 1
            ;;
        *)
            info "start: $BIN_DIR/fleet-host run"
            return 1
            ;;
    esac
}

install_service_launchd() {
    plist="${HOME}/Library/LaunchAgents/ai.agentfleet.host.plist"
    mkdir -p "$(dirname "$plist")"
    cat >"$plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>ai.agentfleet.host</string>
  <key>ProgramArguments</key>
  <array>
    <string>${BIN_DIR}/fleet-host</string>
    <string>run</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict>
</plist>
EOF
    info "launchd plist: $plist  (launchctl load $plist)"
}

install_service_rcd() {
    if [ "$(id -u)" -ne 0 ]; then
        info "FreeBSD rc.d unit skipped (not root). Run: fleet-host run"
        return 0
    fi
    mkdir -p /usr/local/etc/rc.d
    cat >/usr/local/etc/rc.d/fleet-host <<EOF
#!/bin/sh
# PROVIDE: fleet_host
# REQUIRE: NETWORKING
# KEYWORD: shutdown
. /etc/rc.subr
name="fleet_host"
rcvar="fleet_host_enable"
command="${BIN_DIR}/fleet-host"
command_args="run"
pidfile="/var/run/fleet-host.pid"
load_rc_config \$name
run_rc_command "\$1"
EOF
    chmod 755 /usr/local/etc/rc.d/fleet-host
    info "rc.d script: /usr/local/etc/rc.d/fleet-host  (sysrc fleet_host_enable=YES)"
}

# --- main ---
info "Agent Fleet worker installer"
info "  platform: $PLATFORM"
info "  origin:   $ORIGIN"
info "  prefix:   $PREFIX"

if ! install_bins_from_cp; then
    if [ "${FLEET_WORKER_BUILD_FROM_SOURCE:-}" = 1 ]; then
        install_bins_from_source
    else
        die "no $PLATFORM bins from $ORIGIN; on the CP run: just stage-worker-dist  (or FLEET_WORKER_BUILD_FROM_SOURCE=1 FLEET_REPO=/path/to/agent-fleet FLEET_URL=$ORIGIN sh $0)"
    fi
fi

case $PLATFORM in
    linux-*)
        if have systemctl; then
            install_service_systemd
        else
            info "no systemd; start with: $BIN_DIR/fleet-host run"
        fi
        ;;
    darwin-*)
        install_service_launchd
        ;;
    freebsd-*)
        install_service_rcd
        ;;
esac

case :$PATH: in
    *:"$BIN_DIR":*) ;;
    *) info "add $BIN_DIR to PATH" ;;
esac

if [ -n "${FLEET_PAIR_TOKEN:-}" ]; then
    info "pairing with control plane…"
    "$BIN_DIR/fleet-host" pair --url "$ORIGIN" --token "$FLEET_PAIR_TOKEN"
    # pair is a handshake only. Offline after 30s unless run/heartbeats start.
    start_host_service || true
else
    info "next: fleet-host pair --url $ORIGIN --token <pair-token-from-ui>"
    case ${PLATFORM:-} in
        linux-*)
            info "then start heartbeats: systemctl --user enable --now fleet-host"
            info "  (or: $BIN_DIR/fleet-host run)"
            ;;
        darwin-*)
            info "then start heartbeats: launchctl load ~/Library/LaunchAgents/ai.agentfleet.host.plist"
            info "  (or: $BIN_DIR/fleet-host run)"
            ;;
        freebsd-*)
            info "then start heartbeats: $BIN_DIR/fleet-host run"
            ;;
        *)
            info "then start heartbeats: $BIN_DIR/fleet-host run"
            ;;
    esac
    info "config (host vs container) is local: ~/.config/fleet-host/config.toml"
    info "  run_host / run_container — this machine only"
fi
