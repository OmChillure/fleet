#!/bin/sh
# Agent Fleet worker bootstrap — POSIX. Same file on every Unix/WSL/Git-Bash host.
# Downloads the platform installer from:
#   A) official base (FLEET_OFFICIAL_BASE), or
#   B) a control plane URL the user supplies (FLEET_URL).
#
# Usage:
#   curl -fsSL https://<cp>/install-worker.sh | FLEET_URL=https://<cp> FLEET_PAIR_TOKEN=<token> sh
#   curl -fsSL https://<cp>/install-worker.sh | FLEET_URL=https://<cp> sh -s -- verify
#   curl -fsSL https://<cp>/install-worker.sh | FLEET_URL=https://<cp> sh -s -- repair
#   fetch -o - https://<cp>/install-worker.sh | FLEET_URL=https://<cp> FLEET_PAIR_TOKEN=<token> sh
#   FLEET_URL=http://127.0.0.1:8080 FLEET_PAIR_TOKEN=<token> sh install-worker.sh
#   FLEET_DETECT_ONLY=1 sh install-worker.sh   # print platform id and exit
#
# Windows native: use install-worker.ps1 (this file is POSIX).
set -eu

OFFICIAL_DEFAULT="${FLEET_OFFICIAL_BASE:-}"

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

# Warn on loopback. Off-box (GET /health fails) dies before any other fetch.
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

# Download URL to stdout. Named http_get so `command -v fetch` sees FreeBSD fetch(1).
http_get() {
    _url=$1
    if have curl; then
        curl -fsSL "$_url"
    elif have wget; then
        wget -qO- "$_url"
    elif have fetch; then
        # FreeBSD: fetch -o dest url  (dest=- is stdout)
        fetch -o - "$_url"
    else
        die "need curl, wget, or fetch to download $_url"
    fi
}

# --- platform id (must match scripts/install-worker/<id>.sh) ---
detect_platform() {
    os=$(uname -s 2>/dev/null || echo unknown)
    arch=$(uname -m 2>/dev/null || echo unknown)
    kern=$(uname -r 2>/dev/null || echo)

    case $os in
        Linux) sys=linux ;;
        Darwin) sys=darwin ;;
        FreeBSD) sys=freebsd ;;
        MINGW*|MSYS*|CYGWIN*) sys=windows ;;
        *) die "unsupported uname -s: $os" ;;
    esac

    case $arch in
        x86_64|amd64) cpu=x86_64 ;;
        aarch64|arm64) cpu=aarch64 ;;
        armv7l|armv7|armhf) cpu=armv7 ;;
        riscv64) cpu=riscv64 ;;
        ppc64le) cpu=ppc64le ;;
        s390x) cpu=s390x ;;
        *) die "unsupported uname -m: $arch" ;;
    esac

    # WSL2 uses the Linux glibc/musl script, not a separate id.
    musl=0
    if [ "$sys" = linux ]; then
        if have ldd; then
            if ldd /bin/sh 2>/dev/null | grep -qi musl; then
                musl=1
            fi
        fi
        if [ -f /etc/alpine-release ]; then
            musl=1
        fi
    fi

    case $sys in
        linux)
            if [ "$musl" = 1 ]; then
                case $cpu in
                    x86_64) printf '%s\n' linux-x86_64-musl ;;
                    aarch64) printf '%s\n' linux-aarch64-musl ;;
                    armv7) printf '%s\n' linux-armv7-musl ;;
                    *) die "no musl installer for $cpu" ;;
                esac
            else
                case $cpu in
                    x86_64) printf '%s\n' linux-x86_64 ;;
                    aarch64) printf '%s\n' linux-aarch64 ;;
                    armv7) printf '%s\n' linux-armv7 ;;
                    *) die "no glibc installer for $cpu (see docs/components/worker-host)" ;;
                esac
            fi
            ;;
        darwin)
            case $cpu in
                aarch64) printf '%s\n' darwin-arm64 ;;
                x86_64) printf '%s\n' darwin-x86_64 ;;
                *) die "no macOS installer for $cpu" ;;
            esac
            ;;
        freebsd)
            case $cpu in
                x86_64) printf '%s\n' freebsd-amd64 ;;
                *) die "no FreeBSD installer for $cpu" ;;
            esac
            ;;
        windows)
            case $cpu in
                x86_64) printf '%s\n' windows-x86_64 ;;
                aarch64) printf '%s\n' windows-arm64 ;;
                *) die "no Windows installer for $cpu" ;;
            esac
            ;;
    esac
}

normalize_url() {
    u=$1
    case $u in
        http://*|https://*) ;;
        *) u="https://$u" ;;
    esac
    printf '%s\n' "$u" | sed 's:/*$::'
}

prompt_origin() {
    if [ -n "${FLEET_URL:-}" ]; then
        normalize_url "$FLEET_URL"
        return
    fi
    if [ ! -t 0 ]; then
        die "piped install requires FLEET_URL (control plane) or FLEET_OFFICIAL_BASE.
  Example: curl -fsSL http://127.0.0.1:8080/install-worker.sh | FLEET_URL=http://127.0.0.1:8080 sh
  FreeBSD: fetch -o - http://127.0.0.1:8080/install-worker.sh | FLEET_URL=http://127.0.0.1:8080 sh"
    fi
    info "Install this worker from:"
    if [ -n "$OFFICIAL_DEFAULT" ]; then
        info "  1) Official  ($OFFICIAL_DEFAULT)"
    else
        info "  1) Official  (not configured — you will be asked for a URL)"
    fi
    info "  2) Control plane (type hostname or URL)"
    printf 'Choice [1/2]: '
    read -r choice || die "no choice"
    case $choice in
        1|"")
            if [ -n "$OFFICIAL_DEFAULT" ]; then
                normalize_url "$OFFICIAL_DEFAULT"
            else
                printf 'Official / download URL: '
                read -r u || die "no url"
                [ -n "$u" ] || die "empty url"
                normalize_url "$u"
            fi
            ;;
        2)
            printf 'Control plane URL or domain: '
            read -r u || die "no url"
            [ -n "$u" ] || die "empty url"
            normalize_url "$u"
            ;;
        *)
            die "enter 1 or 2"
            ;;
    esac
}

PLATFORM=$(detect_platform)
if [ "${FLEET_DETECT_ONLY:-}" = 1 ]; then
    info "$PLATFORM"
    exit 0
fi

ORIGIN=$(prompt_origin)
export FLEET_URL="$ORIGIN"
export FLEET_INSTALL_ORIGIN="$ORIGIN"
export FLEET_PLATFORM="$PLATFORM"
# UI one-liner sets FLEET_PAIR_TOKEN on this sh; the platform child must see it.
export FLEET_PAIR_TOKEN
# install (default), verify, or repair. sh -s -- verify passes the argument
# through this pipe; the platform installer and _unix_common.sh honor it.
FLEET_ACTION=${1:-${FLEET_ACTION:-install}}
case $FLEET_ACTION in
    install|verify|repair) ;;
    *) die "action must be install, verify, or repair" ;;
esac
export FLEET_ACTION

info "platform: $PLATFORM"
info "source:   $ORIGIN"
warn_loopback_origin "$ORIGIN"

case $PLATFORM in
    windows-*)
        script="$ORIGIN/install/worker/${PLATFORM}.ps1"
        die "this POSIX bootstrap cannot run a native Windows installer.
Download and run:
  irm $ORIGIN/install-worker.ps1 | iex
or:
  $script"
        ;;
    *)
        script="$ORIGIN/install/worker/${PLATFORM}.sh"
        ;;
esac

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
info "downloading $PLATFORM installer"
if ! http_get "$script" >"$tmp"; then
    die "failed to download $script"
fi
# Reject HTML error pages pretending to be a script.
if head -n 1 "$tmp" | grep -q '<'; then
    die "download of $script does not look like a shell script (is the control plane up?)"
fi
chmod +x "$tmp"
info "running $script"
sh "$tmp"
