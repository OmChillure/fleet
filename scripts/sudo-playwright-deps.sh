#!/usr/bin/env bash
# Not a second install. Chromium OS libs are in the one install script.
echo "ERROR: do not run $0." >&2
echo "Run crates/fleet-sandbox-driver/sandbox-lab/scripts/install-full-suite-deps.sh" >&2
echo "(no arguments; ENABLE_BROWSER_E2E=1 in repo-root .env)." >&2
exit 2
