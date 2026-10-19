#!/usr/bin/env bash
# Smoke test for a locally loaded image: runs as nonroot, /healthz answers, and the
# static client (dist/index.html) is present.
# Usage: scripts/smoke-test-image.sh IMAGE
set -euo pipefail

image="${1:?usage: $0 IMAGE}"
tmp="$(mktemp -d)"

user="$(docker image inspect --format '{{.Config.User}}' "${image}")"
echo "image user: ${user}"
case "${user}" in 65532:65532|65532|nonroot|nonroot:nonroot) ;; *) echo "image must run as nonroot" >&2; exit 1 ;; esac

docker run -d --name obs -p 8080:8080 "${image}"
trap 'docker logs obs || true; docker rm -f obs >/dev/null 2>&1 || true; rm -rf "${tmp}"' EXIT

for i in $(seq 1 30); do
  if curl -fsS --max-time 2 http://127.0.0.1:8080/healthz; then
    echo
    echo "healthz OK after ${i} attempt(s)"
    break
  fi
  if [ "$(docker inspect --format '{{.State.Running}}' obs)" != "true" ]; then
    echo "container exited early" >&2
    exit 1
  fi
  if [ "${i}" -eq 30 ]; then
    echo "healthz did not become ready" >&2
    exit 1
  fi
  sleep 1
done

# The static bundle must have been copied into the image.
docker cp obs:/app/dist/index.html "${tmp}/index.html"
grep -qi '<html' "${tmp}/index.html"
