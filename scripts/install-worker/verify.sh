#!/bin/sh
# Same installer as install-worker.sh. Kept so an older URL still works.
#   sh verify.sh            # diagnose, when this file sits next to _unix_common.sh
#   sh verify.sh repair
#   curl -fsSL <control-plane>/install/worker/verify.sh | sh
#
# Piped into sh, $0 is the shell. dirname of that is the current directory,
# not a path baked into this file. Do not source ./_unix_common.sh in that case.
export FLEET_ACTION=${1:-${FLEET_ACTION:-verify}}

case $0 in
    */verify.sh|verify.sh)
        _dir=$(CDPATH= cd -- "$(dirname "$0")" && pwd) || _dir=
        if [ -n "$_dir" ] && [ -f "$_dir/_unix_common.sh" ]; then
            # shellcheck disable=SC1091
            . "$_dir/_unix_common.sh"
        fi
        ;;
esac

_origin=${FLEET_URL:-}
if [ -z "$_origin" ]; then
    # The control plane replaces this token when it serves the file.
    _origin='@fleet-verify-origin@'
fi
case $_origin in
    @fleet-verify-origin@|"")
        echo "verify: piped into the shell, and FLEET_URL is not set." >&2
        echo "curl -fsSL <control-plane>/install/worker/install-worker.sh | FLEET_URL=<control-plane> sh -s -- verify" >&2
        exit 1
        ;;
esac
_origin=${_origin%/}
export FLEET_URL=$_origin
_tmp=$(mktemp)
trap 'rm -f "$_tmp"' EXIT
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$_origin/install/worker/_unix_common.sh" -o "$_tmp"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$_tmp" "$_origin/install/worker/_unix_common.sh"
else
    echo "verify: need curl or wget to download the installer" >&2
    exit 1
fi
# shellcheck disable=SC1090
. "$_tmp"
