#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$ROOT"

cargo test -p shore-llm claude_code
cargo test -p shore-daemon http::mcp
cargo test -p shore-daemon mcp_session
cargo test -p shore-daemon config_check

if [[ "${SHORE_CLAUDE_CODE_LIVE:-0}" == "1" ]]; then
  export MCP_HTTP_PORT="${MCP_HTTP_PORT:-9998}"
  export MCP_HTTP_LOG="${MCP_HTTP_LOG:-/tmp/shore-claude-code-mcp-http.log}"

  python3 dev/spikes/claude-code-probe/mcp_http_server.py &
  server_pid=$!
  cleanup() {
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  }
  trap cleanup EXIT

  sleep 0.5
  cargo test -p shore-llm --test claude_code_live -- --ignored --nocapture --test-threads=1
fi
