#!/usr/bin/env bash
# Smoke test for a locally loaded region agent image: runs as nonroot, ships the
# Overpass and osmium tools, /healthz answers, and other routes need the token.
# No network: no region is imported.
# Usage: scripts/smoke-test-regions.sh IMAGE
set -euo pipefail

image="${1:?usage: $0 IMAGE}"
name="obs-regions-smoke-$$"
token="smoke-$$"
trap 'docker logs "${name}" 2>/dev/null || true; docker rm -f "${name}" >/dev/null 2>&1 || true' EXIT

user="$(docker image inspect --format '{{.Config.User}}' "${image}")"
echo "image user: ${user}"
[ "${user}" = "65532:65532" ] || { echo "image must run as 65532:65532" >&2; exit 1; }

# The import and query tools must be present and runnable.
docker run --rm --entrypoint osmium "${image}" --version | head -n1
for tool in osm3s_query update_database; do
  docker run --rm --entrypoint test "${image}" -x "/opt/overpass/bin/${tool}" \
    || { echo "missing ${tool}" >&2; exit 1; }
done

docker run -d --name "${name}" -e OBS_REGIONS_TOKEN="${token}" -p 127.0.0.1::8090 "${image}"
port="$("$(dirname "${BASH_SOURCE[0]}")/wait-healthz.sh" "${name}" 8090)"

code="$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/regions")"
[ "${code}" = "401" ] || { echo "/regions without the token answered ${code}, want 401" >&2; exit 1; }
curl -fsS -H "Authorization: Bearer ${token}" "http://127.0.0.1:${port}/regions" | grep -q '"regions":\[\]'
# The data volume is writable by the agent's user.
docker exec "${name}" test -w /data
echo "region agent smoke test passed"
