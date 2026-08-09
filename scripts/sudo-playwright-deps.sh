#!/usr/bin/env bash
# OS libraries for headless Chromium. Needs root on Linux.
# Run: sudo ./scripts/sudo-playwright-deps.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-${ROOT}/.tmp/ms-playwright}"
if [[ "$(id -u)" -ne 0 ]]; then
  echo "re-run as root: sudo $0" >&2
  exit 2
fi
python3 -m playwright install-deps chromium
echo "playwright OS deps installed"
