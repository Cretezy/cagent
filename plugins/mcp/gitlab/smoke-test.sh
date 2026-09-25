#!/usr/bin/env bash
set -euo pipefail

image="${1:?usage: smoke-test.sh IMAGE}"
runtime="${OCI_RUNTIME:-docker}"

request='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"cagent-image-smoke-test","version":"1"}}}'
response="$(
  printf '%s\n' "$request" |
    "$runtime" run --rm -i --network none \
      -e GITLAB_PERSONAL_ACCESS_TOKEN=smoke-test-token \
      -e GITLAB_API_URL=https://gitlab.example.test/api/v4 \
      -e GITLAB_PERMISSION_MODE=readonly \
      "$image"
)"

if [[ ! "$response" =~ \"id\"[[:space:]]*:[[:space:]]*1 ]] ||
   [[ ! "$response" =~ \"protocolVersion\"[[:space:]]*: ]]; then
  printf 'GitLab MCP initialize failed; response was:\n%s\n' "$response" >&2
  exit 1
fi

echo "GitLab MCP image smoke test passed: $image"
