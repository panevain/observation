#!/usr/bin/env bash
# Waits up to 30 s for a container's /healthz to answer on a loopback host port; fails at once
# if the container exits. Used by the smoke tests.
# Usage: scripts/wait-healthz.sh CONTAINER HOST_PORT
set -euo pipefail

name="${1:?usage: $0 CONTAINER HOST_PORT}"
port="${2:?usage: $0 CONTAINER HOST_PORT}"
for i in $(seq 1 30); do
  if curl -fsS --max-time 2 -o /dev/null "http://127.0.0.1:${port}/healthz"; then
    echo "healthz OK after ${i} attempt(s)"
    exit 0
  fi
  [ "$(docker inspect --format '{{.State.Running}}' "${name}")" = true ] || { echo "container exited early" >&2; exit 1; }
  sleep 1
done
echo "healthz did not become ready" >&2
exit 1
