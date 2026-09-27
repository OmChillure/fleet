#!/bin/sh
# Same installer as install-worker.sh. Kept so an older command still works.
#   sh verify.sh            # diagnose
#   sh verify.sh repair     # switch/restart with the same token
export FLEET_ACTION=${1:-${FLEET_ACTION:-verify}}
# shellcheck disable=SC1091
. "$(CDPATH= cd -- "$(dirname "$0")" && pwd)/_unix_common.sh"
