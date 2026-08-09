#!/usr/bin/env bash
# Get tools this host needs to run every test. Not a second install guide.
# Install packages still come from install-full-suite-deps.sh (no flags).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "${ROOT}"
export PATH="${HOME}/.cargo/bin:${HOME}/.grok/bin:${PATH}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${ROOT}/target}"

ok() { printf 'OK  %s\n' "$*"; }
need() { command -v "$1" >/dev/null 2>&1; }

# .env for compose Postgres + browser e2e (gitignored)
if [[ ! -f "${ROOT}/.env" ]]; then
  if [[ -f "${ROOT}/.env.example" ]]; then
    cp "${ROOT}/.env.example" "${ROOT}/.env"
  else
    : >"${ROOT}/.env"
  fi
fi
# Force the compose DB that is already on :5433 (example defaults to :5432).
if grep -qE '^DATABASE_URL=' "${ROOT}/.env"; then
  tmp="$(mktemp)"
  sed 's|^DATABASE_URL=.*|DATABASE_URL=postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable|' \
    "${ROOT}/.env" >"${tmp}"
  mv "${tmp}" "${ROOT}/.env"
else
  echo 'DATABASE_URL=postgres://fleet:fleet@127.0.0.1:5433/fleet?sslmode=disable' >>"${ROOT}/.env"
fi
grep -qE '^ENABLE_BROWSER_E2E=' "${ROOT}/.env" || echo 'ENABLE_BROWSER_E2E=1' >>"${ROOT}/.env"
grep -qE '^PROFILE=' "${ROOT}/.env" || echo 'PROFILE=full' >>"${ROOT}/.env"
grep -qE '^JWT_SECRET=.{32,}' "${ROOT}/.env" || \
  echo 'JWT_SECRET=dev-change-me-to-a-long-random-secret-at-least-32-chars' >>"${ROOT}/.env"
ok ".env DATABASE_URL → compose :5433, ENABLE_BROWSER_E2E=1"

if ! need dx; then
  echo "==> cargo install dioxus-cli 0.7.3 (matches workspace dioxus pin)"
  if ! cargo install dioxus-cli --version 0.7.3 --locked; then
    echo "WARN: dx install failed (often OpenSSL 4 vs openssl-sys). Kind UI image is enough for e2e." >&2
    echo "      Host UI: install libssl-dev matching OpenSSL 3, or use agent-fleet-ui:local." >&2
  fi
fi
if need dx; then
  ok "dx $(dx --version 2>/dev/null || true)"
else
  echo "WARN: dx not on PATH — use Kind UI or fix openssl and re-run bootstrap" >&2
fi

if ! python3 -c "from playwright.sync_api import sync_playwright" >/dev/null 2>&1; then
  python3 -m pip install --user playwright || python3 -m pip install --user --break-system-packages playwright
fi
export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-${ROOT}/.tmp/ms-playwright}"
mkdir -p "${PLAYWRIGHT_BROWSERS_PATH}"
if ! python3 -c "from playwright.sync_api import sync_playwright; p=sync_playwright().start(); p.chromium.launch(headless=True).close(); p.stop()" >/dev/null 2>&1; then
  echo "==> playwright install chromium → ${PLAYWRIGHT_BROWSERS_PATH}"
  python3 -m playwright install chromium
fi
ok "playwright chromium"

if ! pg_isready -h 127.0.0.1 -p 5433 -U fleet -d fleet >/dev/null 2>&1; then
  echo "==> starting compose postgres"
  docker compose up -d postgres
  for _ in $(seq 1 40); do
    pg_isready -h 127.0.0.1 -p 5433 -U fleet -d fleet >/dev/null 2>&1 && break
    sleep 1
  done
fi
ok "postgres :5433"

echo "Bootstrap done. Next: rebuild Kind images (see run-every-test.sh --prepare-kind) then ./scripts/run-every-test.sh"
