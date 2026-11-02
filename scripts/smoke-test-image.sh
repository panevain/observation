#!/usr/bin/env bash
# Smoke test for a locally loaded image: runs as nonroot, /healthz answers, and the
# static client (dist/index.html) is present.
# Usage: scripts/smoke-test-image.sh IMAGE
set -euo pipefail

image="${1:?usage: $0 IMAGE}"
tmp="$(mktemp -d)"
name="obs-smoke-$$"
trap 'docker logs "${name}" 2>/dev/null || true; docker rm -f "${name}" >/dev/null 2>&1 || true; rm -rf "${tmp}"' EXIT

user="$(docker image inspect --format '{{.Config.User}}' "${image}")"
echo "image user: ${user}"
case "${user}" in 65532:65532|65532|nonroot|nonroot:nonroot) ;; *) echo "image must run as nonroot" >&2; exit 1 ;; esac

# A random free host port on loopback, so concurrent runs and a local server do not clash.
docker run -d --name "${name}" -p 127.0.0.1::8080 "${image}"
port="$(docker port "${name}" 8080/tcp | head -n1 | sed 's/.*://')"

"$(dirname "${BASH_SOURCE[0]}")/wait-healthz.sh" "${name}" "${port}"

# The static bundle must have been copied into the image.
docker cp "${name}:/app/dist/index.html" "${tmp}/index.html"
grep -qi '<html' "${tmp}/index.html"
