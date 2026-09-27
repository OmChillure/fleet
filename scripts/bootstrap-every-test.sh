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
  echo "ERROR: dx (Dioxus CLI) is not on PATH." >&2
  echo "       Run scripts/install-full-suite-deps.sh" >&2
  echo "       (no arguments; PROFILE/ENABLE_* in repo-root .env)." >&2
  echo "       Do not WARN-and-continue. Do not use Kind UI as a substitute for host dx." >&2
  exit 1
fi
ok "dx $(dx --version 2>/dev/null || true)"

export PLAYWRIGHT_BROWSERS_PATH="${PLAYWRIGHT_BROWSERS_PATH:-${ROOT}/.tmp/ms-playwright}"
if ! python3 -c "from playwright.sync_api import sync_playwright; p=sync_playwright().start(); p.chromium.launch(headless=True).close(); p.stop()" >/dev/null 2>&1; then
  echo "ERROR: playwright chromium is not ready." >&2
  echo "       Run scripts/install-full-suite-deps.sh" >&2
  echo "       (no arguments; ENABLE_BROWSER_E2E=1 in repo-root .env). Do not pip/apt around it." >&2
  exit 1
fi
ok "playwright chromium"

if ! pg_isready -h 127.0.0.1 -p 5433 -U fleet -d fleet >/dev/null 2>&1; then
  echo "==> starting compose postgres"
  FLEET_GIT_SHA="$(bash "${ROOT}/scripts/tree-id.sh")" docker compose up -d postgres
  ready=0
  for _ in $(seq 1 40); do
    if pg_isready -h 127.0.0.1 -p 5433 -U fleet -d fleet >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 1
  done
  if [[ "${ready}" -ne 1 ]]; then
    echo "ERROR: postgres :5433 did not become ready after docker compose up." >&2
    exit 1
  fi
fi
ok "postgres :5433"

python3 "${ROOT}/tests/browser/stack_identity.py" --tools
echo "Bootstrap done. Next: rebuild Kind images (see run-every-test.sh --prepare-kind) then ./scripts/run-every-test.sh"
