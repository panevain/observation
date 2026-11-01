#!/usr/bin/env bash
# Runs the headless-browser smoke test (tests/browser/smoke.cjs) against an image: starts it on
# a random loopback port, waits for /healthz, then loads the client in Chromium. The image must
# carry a real client build (Dockerfile target runtime-prebuilt with --build-context
# client-dist=<a trunk dist/>, or runtime), not CI's stub dist/.
# Usage: scripts/browser-smoke-test.sh IMAGE   (CHROME_PATH and OUT_DIR as in smoke.cjs)
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

image="${1:?usage: $0 IMAGE}"
name="obs-browser-$$"
trap 'docker logs "${name}" > "${OUT_DIR:-tests/browser/out}/server.log" 2>&1 || true; docker rm -f "${name}" >/dev/null 2>&1 || true' EXIT
mkdir -p "${OUT_DIR:-tests/browser/out}"

docker run -d --name "${name}" -p 127.0.0.1::8080 "${image}" >/dev/null
port="$(docker port "${name}" 8080/tcp | head -n1 | sed 's/.*://')"
for i in $(seq 1 30); do
  if curl -fsS --max-time 2 "http://127.0.0.1:${port}/healthz" >/dev/null; then break; fi
  [ "$(docker inspect --format '{{.State.Running}}' "${name}")" = true ] || { echo "container exited early" >&2; exit 1; }
  [ "${i}" -lt 30 ] || { echo "/healthz did not answer" >&2; exit 1; }
  sleep 1
done

(cd tests/browser && npm ci --ignore-scripts --no-audit --no-fund)
OBS_URL="http://127.0.0.1:${port}" node tests/browser/smoke.cjs
