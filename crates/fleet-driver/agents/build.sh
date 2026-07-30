#!/usr/bin/env bash
# Build all Alpine agent images. Run from anywhere; resolves paths relative to this script.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "${ROOT}"

BASE_TAG="${BASE_TAG:-agent-fleet-base:alpine}"
CODEX_TAG="${CODEX_TAG:-agent-fleet-codex:alpine}"
OPENCODE_TAG="${OPENCODE_TAG:-agent-fleet-opencode:alpine}"
GROK_TAG="${GROK_TAG:-agent-fleet-grok:alpine}"
PI_TAG="${PI_TAG:-agent-fleet-pi:alpine}"

CODEX_ACP_VERSION="${CODEX_ACP_VERSION:-1.1.7}"
OPENCODE_VERSION="${OPENCODE_VERSION:-1.18.9}"
GROK_VERSION="${GROK_VERSION:-}"
PI_VERSION="${PI_VERSION:-0.83.0}"
PI_ACP_VERSION="${PI_ACP_VERSION:-0.0.33}"
PLATFORM="${PLATFORM:-}"

build() {
  local tag="$1" file="$2" context="$3"
  shift 3
  local args=(-t "${tag}" -f "${file}")
  if [ -n "${PLATFORM}" ]; then
    args+=(--platform "${PLATFORM}")
  fi
  args+=("$@")
  echo "==> docker build ${tag}"
  docker build "${args[@]}" "${context}"
}

build "${BASE_TAG}" "${ROOT}/base/Dockerfile" "${ROOT}/base"

build "${CODEX_TAG}" "${ROOT}/codex/Dockerfile" "${ROOT}/codex" \
  --build-arg "BASE_IMAGE=${BASE_TAG}" \
  --build-arg "CODEX_ACP_VERSION=${CODEX_ACP_VERSION}"

build "${OPENCODE_TAG}" "${ROOT}/opencode/Dockerfile" "${ROOT}/opencode" \
  --build-arg "BASE_IMAGE=${BASE_TAG}" \
  --build-arg "OPENCODE_VERSION=${OPENCODE_VERSION}"

grok_args=(--build-arg "BASE_IMAGE=${BASE_TAG}")
if [ -n "${GROK_VERSION}" ]; then
  grok_args+=(--build-arg "GROK_VERSION=${GROK_VERSION}")
fi
build "${GROK_TAG}" "${ROOT}/grok/Dockerfile" "${ROOT}/grok" "${grok_args[@]}"

build "${PI_TAG}" "${ROOT}/pi/Dockerfile" "${ROOT}/pi" \
  --build-arg "BASE_IMAGE=${BASE_TAG}" \
  --build-arg "PI_VERSION=${PI_VERSION}" \
  --build-arg "PI_ACP_VERSION=${PI_ACP_VERSION}"

echo
echo "Built:"
echo "  ${BASE_TAG}"
echo "  ${CODEX_TAG}"
echo "  ${OPENCODE_TAG}"
echo "  ${GROK_TAG}"
echo "  ${PI_TAG}"
