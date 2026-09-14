#!/usr/bin/env bash
# Leftover compatibility wrapper.
# Canonical path: scripts/install-full-suite-deps.sh
set -euo pipefail
exec "$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)/scripts/install-full-suite-deps.sh" "$@"
