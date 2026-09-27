#!/bin/sh
# Shared Unix installer. Sourced by scripts/install-worker/<platform>.sh
# Expects: FLEET_PLATFORM, FLEET_URL (or FLEET_INSTALL_ORIGIN), FLEET_PAIR_TOKEN
# Optional: FLEET_WORKER_BUILD_FROM_SOURCE=1, FLEET_PREFIX, FLEET_REPO
set -eu

ORIGIN="${FLEET_INSTALL_ORIGIN:-${FLEET_URL:-}}"
PLATFORM="${FLEET_PLATFORM:-}"
PREFIX="${FLEET_PREFIX:-}"
REPO="${FLEET_REPO:-}"

info() { printf '%s\n' "$*"; }
err() { printf 'worker install: %s\n' "$*" >&2; }
die() { err "$*"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Host of an http(s) URL. IPv6 in brackets ([::1]:8080) or bare ::1.
# userinfo (user@ / user:pass@) is dropped; last @, same as host_from_url.
url_host() {
    _u=$1
    case $_u in
        *://*) _u=${_u#*://} ;;
    esac
    _u=${_u%%/*}
    case $_u in
        *@*) _u=${_u##*@} ;;
    esac
    case $_u in
        \[*)
            _u=${_u#\[}
            _u=${_u%%]*}
            ;;
        *)
            case $_u in
                *:*)
                    _rest=${_u#*:}
                    case $_rest in
                        *:*) ;;
                        *) _u=${_u%%:*} ;;
                    esac
                    ;;
            esac
            ;;
    esac
    printf '%s\n' "$_u"
}

# 127/8, localhost, or ::1 — another computer cannot reach this URL.
host_is_loopback() {
    _h=$(printf '%s' "$1" | tr 'A-Z' 'a-z')
    _h=${_h#[}
    _h=${_h%]}
    case $_h in
        localhost|::1) return 0 ;;
        127.*)
            _a=${_h#127.}
            _b=${_a#*.}
            [ "$_b" = "$_a" ] && return 1
            _c=${_b#*.}
            [ "$_c" = "$_b" ] && return 1
            case $_c in *.*) return 1 ;; esac
            _o1=${_a%%.*}
            _o2=${_b%%.*}
            _o3=$_c
            for _p in "$_o1" "$_o2" "$_o3"; do
                case $_p in
                    ''|*[!0-9]*) return 1 ;;
                esac
                [ "$_p" -le 255 ] || return 1
            done
            return 0
            ;;
    esac
    return 1
}

probe_health() {
    _url=$1
    if have curl; then
        curl -fsSL "$_url" || return 1
    elif have wget; then
        wget -qO- "$_url" || return 1
    elif have fetch; then
        fetch -o - "$_url" || return 1
    else
        return 1
    fi
}

# Warn on loopback. Off-box (GET /health fails) dies before mkdir or bin fetch.
# Same-box /health may continue (local fleet-dev).
warn_loopback_origin() {
    _origin=$1
    if ! host_is_loopback "$(url_host "$_origin")"; then
        return 0
    fi
    err "FLEET_URL $_origin is loopback. Another computer needs a LAN or public control-plane URL."
    if probe_health "$_origin/health" >/dev/null; then
        return 0
    fi
    die "refusing to pair: $_origin is loopback and this host is not the control plane"
}

# install requires a URL, a platform, and a pair token (unless already paired).
# verify and repair are the same script with another argument. They must run
# without a pair token so they cannot mint one.
FLEET_ACTION=${FLEET_ACTION:-install}
case $FLEET_ACTION in
    install|verify|repair) ;;
    *) die "FLEET_ACTION must be install, verify, or repair" ;;
esac
if [ "$FLEET_ACTION" = install ]; then
    [ -n "$ORIGIN" ] || die "FLEET_URL is not set"
    [ -n "$PLATFORM" ] || die "FLEET_PLATFORM is not set"
fi

# Pair token is required for first install. Missing/empty/whitespace must
# fail closed before mkdir or fetch so we never write bins that never pair.
# A leftover empty or whitespace-only machine.token is unpaired (same as
# MachineToken::parse). A non-empty file may re-run without a new pair token.
_pair_token=$(printf '%s' "${FLEET_PAIR_TOKEN:-}" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')
token_file="${FLEET_HOST_CONFIG_DIR:-$HOME/.config/fleet-host}/machine.token"
already_paired=0
if [ -f "$token_file" ]; then
    _existing=$(sed 's/^[[:space:]]*//;s/[[:space:]]*$//' "$token_file")
    [ -n "$_existing" ] && already_paired=1
fi
if [ "$FLEET_ACTION" = install ] && [ -z "$_pair_token" ] && [ "$already_paired" -eq 0 ]; then
    die "FLEET_PAIR_TOKEN is not set"
fi
if [ -n "$_pair_token" ]; then
    export FLEET_PAIR_TOKEN="$_pair_token"
fi

# Same trim as fleet-host refuse_url_switch / normalize_url.
norm_pair_url() {
    printf '%s' "$1" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//;s|/*$||'
}

paired_control_plane_url() {
    _cfg="${FLEET_HOST_CONFIG_DIR:-$HOME/.config/fleet-host}/config.toml"
    [ -f "$_cfg" ] || return 0
    sed -n 's/^[[:space:]]*control_plane_url[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' "$_cfg" | sed -n '1p'
}

# 0 = switch (caller passes --force). Non-zero = keep existing pair.
confirm_unpair_and_pair() {
    _old=$1
    _new=$2
    if [ "${FLEET_PAIR_FORCE:-}" = 1 ]; then
        return 0
    fi
    if [ ! -t 0 ]; then
        die "already paired to $_old; this install is $_new. Re-run in a terminal or set FLEET_PAIR_FORCE=1 to unpair and pair to $_new"
    fi
    info "This machine is already paired to $_old."
    info "Pair to $_new instead? That unpairs the old control plane and rotates the machine token."
    printf 'Unpair and pair to %s? [y/N] ' "$_new" >&2
    read -r _ans || _ans=
    case $_ans in
        y|Y|yes|YES) return 0 ;;
        *) die "kept existing pair to $_old" ;;
    esac
}

if [ "$FLEET_ACTION" = install ]; then
    warn_loopback_origin "$ORIGIN"
fi

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
    # $3=quiet — no progress (tiny .sha256). Bin downloads stay noisy.
    _url=$1
    _dest=$2
    _quiet=${3:-}
    if have curl; then
        if [ -n "$_quiet" ]; then
            curl -fsSL "$_url" -o "$_dest"
        else
            curl -fL --progress-bar "$_url" -o "$_dest"
        fi
    elif have wget; then
        if [ -n "$_quiet" ]; then
            wget -q -O "$_dest" "$_url"
        else
            wget -O "$_dest" "$_url"
        fi
    elif have fetch; then
        # FreeBSD: fetch -o dest url (not dest= — keep dest-leak fix)
        if [ -n "$_quiet" ]; then
            fetch -q -o "$_dest" "$_url"
        else
            fetch -o "$_dest" "$_url"
        fi
    else
        die "need curl, wget, or fetch"
    fi
}

content_length() {
    if have curl; then
        curl -sI -L "$1" | tr -d '\r' | awk 'tolower($1)=="content-length:" {print $2; exit}'
    fi
}

size_note() {
    n=$1
    [ -n "$n" ] || return 0
    awk -v n="$n" 'BEGIN {
        if (n+0 < 1) exit
        if (n >= 1048576) printf " (%.0f MB)", n/1048576
        else if (n >= 1024) printf " (%.0f KB)", n/1024
        else printf " (%s B)", n
    }'
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
    info "this machine is a self-hosted worker"
    info "  fleet-host   stays up; pairs and takes jobs"
    info "  fleet-driver runs each job"
    base="$ORIGIN/install/worker/bin/$PLATFORM"
    ok=0
    for bin in fleet-host fleet-driver; do
        dest="$BIN_DIR/$bin"
        want=""
        info "checking $bin"
        if fetch_to "$base/$bin.sha256" "$dest.sha256.tmp" quiet 2>/dev/null; then
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
            info "local $bin is stale; downloading again"
        fi
        bin_url="$base/$bin"
        if [ -n "$want" ]; then
            bin_url="$base/$bin?h=$want"
        fi
        bytes=$(content_length "$bin_url" || true)
        note=$(size_note "$bytes")
        info "downloading $bin$note"
        if fetch_to "$bin_url" "$dest.tmp"; then
            if [ -n "$want" ]; then
                info "verifying $bin"
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
    info "building fleet-host + fleet-driver from $src"
    (cd "$src" && cargo build --release -p fleet-host -p fleet-driver)
    # mv over a running dest works; cp does not (ETXTBSY).
    cp "$src/target/release/fleet-host" "$BIN_DIR/fleet-host.tmp"
    chmod 755 "$BIN_DIR/fleet-host.tmp"
    mv "$BIN_DIR/fleet-host.tmp" "$BIN_DIR/fleet-host"
    if [ -f "$src/target/release/fleet-driver" ]; then
        cp "$src/target/release/fleet-driver" "$BIN_DIR/fleet-driver.tmp"
        chmod 755 "$BIN_DIR/fleet-driver.tmp"
        mv "$BIN_DIR/fleet-driver.tmp" "$BIN_DIR/fleet-driver"
    fi
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
TimeoutStopSec=60

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

stop_host_service() {
    # Replace-on-disk leaves a running process on the old inode (Linux), and
    # a locked PE cannot be replaced at all (Windows — handled in the .ps1).
    # enable --now is a no-op when the unit is already active.
    case ${PLATFORM:-} in
        linux-*)
            if have systemctl; then
                if [ "$(id -u)" -eq 0 ]; then
                    systemctl stop fleet-host 2>/dev/null || true
                else
                    systemctl --user stop fleet-host 2>/dev/null || true
                fi
            fi
            ;;
        darwin-*)
            plist="${HOME}/Library/LaunchAgents/ai.agentfleet.host.plist"
            uid=$(id -u)
            if have launchctl && [ -f "$plist" ]; then
                launchctl bootout "gui/${uid}" "$plist" 2>/dev/null || true
                launchctl unload "$plist" 2>/dev/null || true
            fi
            ;;
        freebsd-*)
            if [ "$(id -u)" -eq 0 ] && [ -x /usr/local/etc/rc.d/fleet-host ]; then
                /usr/local/etc/rc.d/fleet-host stop 2>/dev/null || true
            fi
            ;;
    esac
}

start_host_service() {
    case ${PLATFORM:-} in
        linux-*)
            if have systemctl; then
                if [ "$(id -u)" -eq 0 ]; then
                    systemctl enable fleet-host
                    systemctl restart fleet-host
                    info "started systemd unit fleet-host (systemctl status fleet-host)"
                    return 0
                fi
                systemctl --user enable fleet-host
                systemctl --user restart fleet-host
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
                /usr/local/etc/rc.d/fleet-host restart 2>/dev/null \
                    || /usr/local/etc/rc.d/fleet-host start
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

# Point config.toml at a control plane without touching machine.token.
write_control_plane_url() {
    _url=$1
    _cfg_dir=${FLEET_HOST_CONFIG_DIR:-$HOME/.config/fleet-host}
    _cfg=$_cfg_dir/config.toml
    _tok=$_cfg_dir/machine.token
    mkdir -p "$_cfg_dir"
    _copy=$(mktemp)
    cp "$_tok" "$_copy"
    _tmp=$(mktemp)
    if [ -f "$_cfg" ]; then
        awk -v url="$_url" '
            BEGIN { done = 0 }
            /^[[:space:]]*control_plane_url[[:space:]]*=/ {
                print "control_plane_url = \"" url "\""
                done = 1
                next
            }
            { print }
            END { if (!done) print "control_plane_url = \"" url "\"" }
        ' "$_cfg" > "$_tmp"
    else
        printf 'control_plane_url = "%s"\n' "$_url" > "$_tmp"
    fi
    mv "$_tmp" "$_cfg"
    if ! cmp -s "$_tok" "$_copy"; then
        rm -f "$_copy"
        die "machine.token changed while writing the URL; not pairing"
    fi
    rm -f "$_copy"
}

# Ping /health. Prints one status word to stdout. Never prints a token.
ping_health() {
    _url=$1
    case $_url in
        http://*|https://*) ;;
        *) printf '%s\n' "not configured"; return ;;
    esac
    if ! have curl && ! have wget && ! have fetch; then
        printf '%s\n' "curl missing"
        return
    fi
    _body=$(probe_health "$_url/health" 2>/dev/null) || _body=
    if [ -z "$_body" ]; then
        printf '%s\n' "unreachable"
        return
    fi
    _status=$(printf '%s' "$_body" | sed -n 's/.*"status"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
    _sha=$(printf '%s' "$_body" | sed -n 's/.*"git_sha"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$_status" ] || _status=ok
    [ -n "$_sha" ] || _sha=unknown
    printf '%s\n' "reachable status=${_status} git_sha=${_sha}"
}

# verify prints the diagnosis. repair (or a yes answer) switches the URL
# when it differs and restarts via start_host_service. Same machine token.
fleet_verify() {
    if [ -z "${PLATFORM:-}" ]; then
        case $(uname -s 2>/dev/null || echo unknown) in
            Linux) PLATFORM=linux-x86_64 ;;
            Darwin) PLATFORM=darwin-arm64 ;;
            FreeBSD) PLATFORM=freebsd-amd64 ;;
        esac
    fi
    _cfg_dir=${FLEET_HOST_CONFIG_DIR:-$HOME/.config/fleet-host}
    _bin=""
    if [ -x "${HOME}/.local/bin/fleet-host" ]; then
        _bin=${HOME}/.local/bin/fleet-host
    elif [ -x /usr/local/bin/fleet-host ]; then
        _bin=/usr/local/bin/fleet-host
    elif have fleet-host; then
        _bin=$(command -v fleet-host)
    fi
    _token=0
    if [ "$already_paired" -eq 1 ]; then
        _token=1
    fi
    _user_unit=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/fleet-host.service
    _sys_unit=/etc/systemd/system/fleet-host.service
    _scope=
    if [ -f "$_user_unit" ]; then _scope=user; fi
    if [ -f "$_sys_unit" ] && { [ "$(id -u)" -eq 0 ] || [ -z "$_scope" ]; }; then
        _scope=system
    fi
    _enabled= _active= _running=0
    if [ -n "$_scope" ] && have systemctl; then
        if [ "$_scope" = user ]; then
            _enabled=$(systemctl --user is-enabled fleet-host 2>/dev/null || true)
            _active=$(systemctl --user is-active fleet-host 2>/dev/null || true)
        else
            _enabled=$(systemctl is-enabled fleet-host 2>/dev/null || true)
            _active=$(systemctl is-active fleet-host 2>/dev/null || true)
        fi
    fi
    if have pgrep && pgrep -x fleet-host >/dev/null 2>&1; then
        _running=1
    fi
    case $_active in
        active|activating) _running=1 ;;
    esac
    _verdict=not_installed
    _code=4
    _next="On Runners → Self-hosted, use Add worker and paste the install command."
    if [ -z "$_bin" ] && [ ! -f "$_cfg_dir/config.toml" ] && [ -z "$_scope" ]; then
        _verdict=not_installed
        _means="the install is gone. Add worker and paste the install command. If the output says token present, that reinstall keeps the same machine. Do not re-pair."
    elif [ -z "$_bin" ]; then
        _verdict=bins_missing
        _means="the install is gone. Add worker and paste the install command. If the output says token present, that reinstall keeps the same machine. Do not re-pair."
        if [ "$_token" -eq 1 ]; then
            _next="The program is gone and the machine token is still here. Re-run install. It keeps this machine. Do not re-pair."
        fi
    elif [ "$_running" -eq 1 ]; then
        _verdict=service_running
        _code=0
        _means="this computer is up. A card that stays offline cannot reach the control plane. Read the plane lines."
        _next="The process is up. The plane lines say whether to restart or switch."
    elif [ "$_enabled" = disabled ] || [ "$_enabled" = masked ]; then
        _verdict=service_disabled
        _code=2
        _means="the install is there and the service will not start at login. Run the next line."
        if [ "$_scope" = user ]; then
            _next="systemctl --user enable --now fleet-host"
        else
            _next="systemctl enable --now fleet-host"
        fi
    else
        _verdict=service_stopped
        _code=3
        _means="the install is there and the service is enabled, but the process is down. Run the next line."
        if [ "$_scope" = user ]; then
            _next="systemctl --user start fleet-host"
        elif [ "$_scope" = system ]; then
            _next="systemctl start fleet-host"
        else
            _next="\"$_bin\" run"
        fi
    fi

    _config=$(paired_control_plane_url || true)
    _config=$(norm_pair_url "$_config")
    _command=$(norm_pair_url "${FLEET_URL:-}")
    _config_ping=$(ping_health "$_config")
    _command_ping=$(ping_health "$_command")
    _config_up=0
    _command_up=0
    case $_config_ping in reachable*) _config_up=1 ;; esac
    case $_command_ping in reachable*) _command_up=1 ;; esac
    _different=0
    if [ -n "$_command" ] && [ "$_command" != "$_config" ]; then
        _different=1
    fi

    info "fleet-host verify"
    if [ -n "$_bin" ]; then info "binary:          $_bin"; else info "binary:          missing"; fi
    if [ "$_token" -eq 1 ]; then info "token:           present"; else info "token:           missing"; fi
    if [ -n "$_config" ]; then info "plane_config:    $_config"; else info "plane_config:    missing"; fi
    info "plane_config_ping: $_config_ping"
    if [ -n "$_command" ]; then
        info "plane_command:   $_command"
        info "plane_command_ping: $_command_ping"
    else
        info "plane_command:   (pass verify on the install script so FLEET_URL is set)"
    fi
    if [ "$_different" -eq 1 ]; then
        info "plane:           different"
    elif [ -n "$_config" ]; then
        info "plane:           same"
    else
        info "plane:           missing"
    fi
    info "verdict:         $_verdict"
    info "means:           $_means"
    info "next:            $_next"

    _offer=0
    _ask=
    _target=
    if [ "$_token" -eq 1 ] && [ "$_verdict" != not_installed ] && [ "$_verdict" != bins_missing ]; then
        _offer=1
        if [ "$_different" -eq 1 ] && [ "$_command_up" -eq 1 ] && [ "$_config_up" -eq 0 ]; then
            _ask="Switch control plane from $_config to $_command and restart with the same token? [y/N] "
            _target=$_command
        elif [ "$_different" -eq 1 ] && [ "$_command_up" -eq 1 ] && [ "$_config_up" -eq 1 ]; then
            _ask="$_config answers and $_command answers. Switch to $_command and restart with the same token? [y/N] "
            _target=$_command
        elif [ "$_different" -eq 1 ] && [ "$_command_up" -eq 0 ] && [ "$_config_up" -eq 1 ]; then
            _ask="$_config answers. $_command did not. Switch anyway and restart with the same token? [y/N] "
            _target=$_command
        elif [ "$_different" -eq 1 ]; then
            _ask="Neither control plane answered. Switch from $_config to $_command anyway? [y/N] "
            _target=$_command
        elif [ "$_config_up" -eq 1 ]; then
            _ask="Restart fleet-host with the same token? The control plane answered, so this does not switch planes. [y/N] "
            _target=$_config
        else
            _ask="The control plane did not answer. Restart fleet-host anyway with the same token? This does not switch planes. [y/N] "
            _target=$_config
        fi
    fi

    _yes=0
    case ${FLEET_VERIFY_REPAIR:-} in
        1|y|yes) _yes=1 ;;
        0|n|no) _yes=0 ;;
        *)
            if [ "$_offer" -eq 1 ] && [ -t 1 ] && [ -r /dev/tty ]; then
                printf '%s' "$_ask" >/dev/tty
                _ans=
                read -r _ans </dev/tty || _ans=
                case $_ans in
                    y|Y|yes|YES) _yes=1 ;;
                esac
            fi
            ;;
    esac
    if [ "$FLEET_ACTION" = repair ]; then
        _yes=1
        _offer=1
        if [ -z "$_target" ]; then
            _target=${_command:-$_config}
        fi
    fi
    if [ "$_yes" -eq 1 ] && [ "$_offer" -eq 1 ] && [ -n "$_target" ]; then
        case $_target in
            http://*|https://*) ;;
            *)
                info "repair:          refused"
                info "repair_next:     No control plane URL to write."
                return "$_code"
                ;;
        esac
        write_control_plane_url "$_target"
        if [ -z "${PREFIX:-}" ]; then
            if [ "$(id -u)" -eq 0 ]; then PREFIX=/usr/local; else PREFIX=${HOME}/.local; fi
            BIN_DIR=${PREFIX}/bin
        fi
        if start_host_service; then
            info "repair:          same token"
            info "repair_url:      $_target"
            return 0
        fi
        info "repair:          url written"
        info "repair_url:      $_target"
        info "repair_next:     The token is unchanged. Start fleet-host run yourself."
        return 3
    fi
    if [ "$_offer" -eq 1 ] && [ "$_yes" -eq 0 ]; then
        info "repair:          offered"
        info "repair_next:     Re-run and answer y, or pass repair. The machine token stays. It does not re-pair."
    fi
    return "$_code"
}

case ${FLEET_ACTION:-install} in
    verify|repair)
        fleet_verify
        exit $?
        ;;
esac

# --- main ---
info "Agent Fleet worker installer"
info "  platform: $PLATFORM"
info "  origin:   $ORIGIN"
info "  prefix:   $PREFIX"

# already_paired / token_file set at the token gate above.

# Stop before replace. A running fleet-host keeps the old inode; `cp` hits
# ETXTBSY; `systemctl enable --now` will not restart an already-active unit.
info "stopping fleet-host if it is running (so the new bins can replace it)"
stop_host_service || true

if ! install_bins_from_cp; then
    if [ "${FLEET_WORKER_BUILD_FROM_SOURCE:-}" = 1 ]; then
        install_bins_from_source
    else
        if [ "$already_paired" = 1 ]; then
            err "bin install failed; restarting the previous fleet-host"
            start_host_service || true
        fi
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
    _old_pair=$(paired_control_plane_url)
    _old_pair=$(norm_pair_url "$_old_pair")
    _new_pair=$(norm_pair_url "$ORIGIN")
    _pair_force=
    if [ "$already_paired" -eq 1 ] && [ -n "$_old_pair" ] && [ "$_old_pair" != "$_new_pair" ]; then
        confirm_unpair_and_pair "$_old_pair" "$_new_pair"
        _pair_force=--force
    fi
    info "pairing with the control plane"
    if [ -n "$_pair_force" ]; then
        "$BIN_DIR/fleet-host" pair --url "$ORIGIN" --token "$FLEET_PAIR_TOKEN" --force
    else
        "$BIN_DIR/fleet-host" pair --url "$ORIGIN" --token "$FLEET_PAIR_TOKEN"
    fi
    info "starting fleet-host (heartbeats; offline after 90s if this fails)"
    start_host_service || true
elif [ "$already_paired" = 1 ]; then
    # We stopped the unit before replacing bins; start the new inode.
    info "already paired; restarting fleet-host so the new bins are what heartbeats"
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
