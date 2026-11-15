#!/usr/bin/env bash
# Smoke test for a locally loaded MCP server image: runs as nonroot, and answers the MCP
# handshake, tools/list and a call that loads the compiled-in demo street over stdio, with no
# network, a read-only filesystem and no capabilities (as compose.yaml runs it). Then over
# Streamable HTTP, read-only with no capabilities and its tokens in a file (OBS_MCP_TOKENS_FILE,
# as a Docker secret would give them; compose.yaml's mcp-http service passes OBS_MCP_TOKENS): /healthz answers, /mcp refuses a missing or wrong bearer token
# and a foreign Origin, answers CORS for an allowed one, and serves the handshake and the demo
# street to a right token.
# Usage: scripts/smoke-test-mcp.sh IMAGE
set -euo pipefail

image="${1:?usage: $0 IMAGE}"
name="obs-mcp-smoke-$$"
http_name="obs-mcp-http-smoke-$$"
tmp="$(mktemp -d)"
trap 'docker rm -f "${name}" "${http_name}" >/dev/null 2>&1 || true; rm -rf "${tmp}"' EXIT

user="$(docker image inspect --format '{{.Config.User}}' "${image}")"
echo "image user: ${user}"
[ "${user}" = "65532:65532" ] || { echo "image must run as 65532:65532" >&2; exit 1; }

# One JSON-RPC message per line (MCP's stdio transport). The server negotiates the
# 2025-11-25 handshake, which MCP clients still send.
requests=(
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke-test","version":"0"}}}'
  '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}'
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"load_demo_area","arguments":{}}}'
)

# -i without -t: a TTY would rewrite the stream. Stdin stays open until every answer is in,
# since the server may drop requests still in flight when it reads end of input.
coproc mcp {
  docker run --rm -i --name "${name}" --network none --read-only --cap-drop ALL \
    --security-opt no-new-privileges "${image}" 2>"${tmp}/stderr"
}
for r in "${requests[@]}"; do printf '%s\n' "${r}" >&"${mcp[1]}"; done

answer=()
got=0
while [ "${got}" -lt 3 ]; do
  IFS= read -r -t 60 line <&"${mcp[0]}" || {
    echo "the server stopped or went quiet with ${got} of 3 answers" >&2
    cat "${tmp}/stderr" >&2
    exit 1
  }
  id="$(jq -r '.id // empty' <<<"${line}")"
  if [[ "${id}" =~ ^[123]$ && -z "${answer[id]:-}" ]]; then
    answer[id]="${line}"
    got=$((got + 1))
  fi
done
exec {mcp[1]}>&-
wait "${mcp_PID}" || true

for id in 1 2 3; do
  if jq -e '.error' <<<"${answer[${id}]}" >/dev/null; then
    echo "request ${id} failed: ${answer[${id}]}" >&2
    exit 1
  fi
done
jq -er '.result.serverInfo.name' <<<"${answer[1]}"
tools="$(jq -r '.result.tools | length' <<<"${answer[2]}")"
echo "tools: ${tools}"
[ "${tools}" -gt 0 ] || { echo "tools/list returned no tools" >&2; exit 1; }
jq -e '.result.isError != true and (.result.structuredContent.building_count // 0) > 0' \
  <<<"${answer[3]}" >/dev/null || { echo "load_demo_area did not load the demo street: ${answer[3]}" >&2; exit 1; }
echo "stdio: passed"

# --- Streamable HTTP ---------------------------------------------------------------------
secret="$(head -c 32 /dev/urandom | base64 | tr -d '\n')"
printf '# smoke test\nsmoke:%s\n' "${secret}" >"${tmp}/tokens"
chmod 0444 "${tmp}/tokens"
docker run -d --name "${http_name}" --read-only --cap-drop ALL \
  --security-opt no-new-privileges -p 127.0.0.1::8090 \
  -v "${tmp}/tokens:/run/secrets/mcp_tokens:ro" \
  -e OBS_MCP_LISTEN=0.0.0.0:8090 -e OBS_MCP_TOKENS_FILE=/run/secrets/mcp_tokens \
  -e OBS_MCP_ALLOWED_ORIGINS=http://localhost:8080 -e RUST_LOG=info "${image}" >/dev/null
port="$(docker port "${http_name}" 8090/tcp | head -n1 | sed 's/.*://')"
base="http://127.0.0.1:${port}"
for _ in $(seq 1 60); do
  curl -fsS "${base}/healthz" >/dev/null 2>&1 && break
  sleep 0.5
done
[ "$(curl -fsS "${base}/healthz")" = "ok" ] || { echo "/healthz did not answer" >&2; docker logs "${http_name}" >&2; exit 1; }

# post BODY [CURL ARGS...]: prints the status code, with the body in ${tmp}/body.
post() {
  local body="$1"
  shift
  curl -sS -o "${tmp}/body" -w '%{http_code}' -X POST "${base}/mcp" \
    -H 'Content-Type: application/json' -H 'Accept: application/json, text/event-stream' \
    "$@" --data "${body}"
}
expect() {
  local want="$1" got="$2" what="$3"
  [ "${got}" = "${want}" ] || {
    echo "${what}: HTTP ${got}, expected ${want}" >&2
    cat "${tmp}/body" >&2
    docker logs "${http_name}" >&2
    exit 1
  }
}
expect 401 "$(post "${requests[0]}")" "no token"
expect 401 "$(post "${requests[0]}" -H "Authorization: Bearer ${secret}x")" "wrong token"
expect 403 "$(post "${requests[0]}" -H "Authorization: Bearer ${secret}" -H 'Origin: https://evil.example')" "foreign Origin"
# A browser's preflight from the allowed origin, which carries no token.
cors="$(curl -sS -o /dev/null -D - -X OPTIONS "${base}/mcp" -H 'Origin: http://localhost:8080' \
  -H 'Access-Control-Request-Method: POST' \
  -H 'Access-Control-Request-Headers: authorization,content-type' | tr -d '\r')"
grep -qix 'access-control-allow-origin: http://localhost:8080' <<<"${cors}" \
  || { echo "no CORS answer for the allowed origin: ${cors}" >&2; exit 1; }
expect 200 "$(post "${requests[0]}" -H "Authorization: Bearer ${secret}")" "initialize"
jq -e '.result.serverInfo.name == "observation-mcp"' "${tmp}/body" >/dev/null \
  || { echo "initialize over HTTP: $(cat "${tmp}/body")" >&2; exit 1; }
expect 200 "$(post "${requests[3]}" -H "Authorization: Bearer ${secret}" -H 'MCP-Protocol-Version: 2025-11-25')" "load_demo_area"
jq -e '.result.isError != true and (.result.structuredContent.building_count // 0) > 0' "${tmp}/body" >/dev/null \
  || { echo "load_demo_area over HTTP: $(cat "${tmp}/body")" >&2; exit 1; }
# Into a variable first: with pipefail, `docker logs | grep -q` could fail on SIGPIPE when grep
# stops at a match, and so hide one.
logs="$(docker logs "${http_name}" 2>&1)"
if grep -qF -- "${secret}" <<<"${logs}"; then
  echo "the server logged a bearer token" >&2
  exit 1
fi
echo "HTTP: passed"
echo "MCP server smoke test passed"
