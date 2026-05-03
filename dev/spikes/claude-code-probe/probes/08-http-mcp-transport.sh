#!/usr/bin/env bash
# Probe 8: does `--mcp-config` accept the HTTP-transport JSON form?
#
# We spin up a Python HTTP MCP server, register it via inline JSON
# config, and verify the model can call the tool roundtrip. This
# determines whether the daemon-side MCP listener can be a plain
# axum HTTP route (preferred) or whether we need to ship a separate
# stdio bridge binary.
set -euo pipefail
source "$(dirname "$0")/_common.sh"

PORT=9998
LOG="$RESULTS_DIR/mcp-http.log"
SERVER_PID_FILE="$RESULTS_DIR/mcp-http.pid"
OUT="$RESULTS_DIR/08-http-mcp.jsonl"

banner "Probe 08: HTTP-transport --mcp-config"

# Start the HTTP MCP server in the background.
MCP_HTTP_PORT="$PORT" MCP_HTTP_LOG="$LOG" python3 "$SPIKE_DIR/mcp_http_server.py" &
echo $! > "$SERVER_PID_FILE"
trap 'kill "$(cat "$SERVER_PID_FILE")" 2>/dev/null || true' EXIT

# Wait for the server to come up.
for _ in $(seq 1 50); do
    if curl -fsS -o /dev/null -X POST -H "Content-Type: application/json" \
        --data '{"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}' \
        "http://127.0.0.1:$PORT/mcp"; then
        break
    fi
    sleep 0.1
done

# Inline MCP config string for HTTP transport.
MCP_CFG=$(cat <<JSON
{"mcpServers":{"shore-http":{"type":"http","url":"http://127.0.0.1:$PORT/mcp"}}}
JSON
)

echo "MCP config: $MCP_CFG"
echo

claude --print \
    --output-format stream-json \
    --input-format stream-json \
    --verbose \
    --no-session-persistence \
    --strict-mcp-config \
    --mcp-config "$MCP_CFG" \
    --allowedTools "mcp__shore-http__ping" \
    --model "$PROBE_MODEL" \
    --tools "" \
    --system-prompt "You are a test fixture. Use the ping tool when asked." \
    < <(echo '{"type":"user","message":{"role":"user","content":"Please ping with the message HTTP-MCP-TOKEN-9933 and report the response."}}') \
    > "$OUT" || true

echo
echo "--- HTTP MCP server saw: ---"
cat "$LOG" 2>/dev/null || echo "(no log)"
echo
echo "--- stream-json analysis ---"
python3 -c "
import json
calls=[]
text_with_pong=False
mcp_status=None
for line in open('$OUT'):
    line=line.strip()
    if not line:continue
    try:ev=json.loads(line)
    except:continue
    if ev.get('type')=='system' and ev.get('subtype')=='init':
        mcp_status=ev.get('mcp_servers')
    elif ev.get('type')=='assistant':
        for b in ev.get('message',{}).get('content',[]):
            if b.get('type')=='tool_use':
                calls.append((b.get('name'),b.get('input')))
            if b.get('type')=='text' and 'pong:' in (b.get('text') or ''):
                text_with_pong=True
print('mcp_servers:',mcp_status)
print('tool_use calls:',calls)
print('assistant text contained pong:',text_with_pong)
"