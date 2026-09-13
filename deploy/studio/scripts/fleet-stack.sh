#!/bin/bash
# Root-owned wrapper. Deploy user may only invoke this via sudo.
#   sudo /usr/local/sbin/fleet-stack load|up SHA|ps SHA|images|rmi IMAGE|backup
set -euo pipefail

STACK=/home/agent-fleet/stack
cd "${STACK}"

valid_sha() {
  [[ "${1:-}" =~ ^[0-9a-f]{7,64}(-[0-9a-f]{6,16})?$ ]]
}

valid_image() {
  [[ "${1:-}" =~ ^agent-fleet-(api|ui):[0-9a-fA-Z._-]+$ ]]
}

cmd="${1:-}"
shift || true

case "${cmd}" in
  load)
    exec /usr/bin/docker load
    ;;
  up)
    sha="${1:?}"
    valid_sha "${sha}" || exit 1
    /usr/bin/docker run --rm \
      -v agent-fleet_fleet_host:/data \
      alpine:3.20@sha256:d9e853e87e55526f6b2917df91a2115c36dd7c696a35be12163d44e6e2a4b6bc \
      chown -R 10001:10001 /data >/dev/null || true
    exec env FLEET_GIT_SHA="${sha}" /usr/bin/docker compose --env-file .env up -d --remove-orphans
    ;;
  ps)
    sha="${1:?}"
    valid_sha "${sha}" || exit 1
    exec env FLEET_GIT_SHA="${sha}" /usr/bin/docker compose --env-file .env ps
    ;;
  images)
    {
      /usr/bin/docker images --format '{{.Repository}}:{{.Tag}}' agent-fleet-api
      /usr/bin/docker images --format '{{.Repository}}:{{.Tag}}' agent-fleet-ui
    }
    exit 0
    ;;
  rmi)
    img="${1:?}"
    valid_image "${img}" || exit 1
    exec /usr/bin/docker rmi "${img}"
    ;;
  backup)
    exec /usr/local/sbin/fleet-backup
    ;;
  *)
    echo "usage: fleet-stack load|up SHA|ps SHA|images|rmi IMAGE|backup" >&2
    exit 2
    ;;
esac
