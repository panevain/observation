#!/usr/bin/env bash
# Smoke test for a locally loaded MCP server image: runs as nonroot, and answers the MCP
# handshake, tools/list and a call that loads the compiled-in demo street over stdio, with no
# network, a read-only filesystem and no capabilities (as compose.yaml runs it).
# Usage: scripts/smoke-test-mcp.sh IMAGE
set -euo pipefail

image="${1:?usage: $0 IMAGE}"
name="obs-mcp-smoke-$$"
tmp="$(mktemp -d)"
trap 'docker rm -f "${name}" >/dev/null 2>&1 || true; rm -rf "${tmp}"' EXIT

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
echo "MCP server smoke test passed"
