#!/usr/bin/env bash
# Waits for a container's /healthz, on the loopback host port Docker published for its
# CONTAINER_PORT, and prints that host port. Gives up once 30 s have passed (each attempt allows
# curl 2 s) and fails at once if the container exits. Used by the smoke tests:
#   port="$(scripts/wait-healthz.sh "${name}" 8080)"
# Usage: scripts/wait-healthz.sh CONTAINER CONTAINER_PORT
set -euo pipefail

name="${1:?usage: $0 CONTAINER CONTAINER_PORT}"
cport="${2:?usage: $0 CONTAINER CONTAINER_PORT}"
port="$(docker port "${name}" "${cport}/tcp" | head -n1 | sed 's/.*://')"
[ -n "${port}" ] || { echo "${name} publishes no host port for ${cport}/tcp" >&2; exit 1; }
deadline=$((SECONDS + 30))
attempt=0
while :; do
  attempt=$((attempt + 1))
  if curl -fsS --max-time 2 -o /dev/null "http://127.0.0.1:${port}/healthz"; then
    echo "healthz OK after ${attempt} attempt(s)" >&2
    echo "${port}"
    exit 0
  fi
  [ "$(docker inspect --format '{{.State.Running}}' "${name}")" = true ] || { echo "container exited early" >&2; exit 1; }
  (( SECONDS < deadline )) || { echo "healthz did not become ready within 30 s" >&2; exit 1; }
  sleep 1
done
