#!/usr/bin/env bash
# Probe 3: model calls our MCP-defined tool and we see it round-trip.
#
# This is the load-bearing test. We expose a single tool `ping` via
# our stdio MCP server, give the model a system prompt that invites
# it to call the tool, and ask it to ping with a recognizable token.
# We then check:
#   - the MCP server log records tools/list and tools/call
#   - the stream-json output contains a tool_use block referencing
#     the MCP tool, followed by a tool_result block, followed by an
#     assistant text block that quotes "pong: ..."
set -euo pipefail
source "$(dirname "$0")/_common.sh"

LOG="$RESULTS_DIR/mcp-ping.log"
: > "$LOG"  # truncate

CFG="$(render_mcp_config)"
OUT="$RESULTS_DIR/03-mcp-tool-call.jsonl"

banner "Probe 03: MCP tool roundtrip"
echo "MCP config: $CFG"
echo "MCP log:    $LOG"

claude --print \
    --output-format stream-json \
    --input-format stream-json \
    --verbose \
    --no-session-persistence \
    --strict-mcp-config \
    --mcp-config "$CFG" \
    --allowedTools "mcp__shore-spike__ping" \
    --model "$PROBE_MODEL" \
    --tools "" \
    --system-prompt "You are a test fixture. You have access to one tool, named 'ping', via an MCP server. When asked to ping, call the tool with the given message and report what you got back." \
    < <(echo '{"type":"user","message":{"role":"user","content":"Please ping with the message SHORE-SPIKE-TOKEN-7741 and tell me the exact response."}}') \
    > "$OUT" || true

echo
echo "--- MCP server saw: ---"
cat "$LOG" || echo "(no log)"
echo
echo "--- stream-json types observed in $OUT: ---"
python3 -c "
import json
calls = []
results = []
text_with_token = False
for line in open('$OUT'):
    line=line.strip()
    if not line: continue
    try: ev = json.loads(line)
    except json.JSONDecodeError: continue
    if ev.get('type') == 'assistant':
        for block in ev.get('message',{}).get('content',[]):
            if block.get('type') == 'tool_use':
                calls.append((block.get('name'), block.get('input')))
            if block.get('type') == 'text' and 'pong:' in block.get('text',''):
                text_with_token = True
    if ev.get('type') == 'user':
        for block in ev.get('message',{}).get('content',[]):
            if isinstance(block, dict) and block.get('type') == 'tool_result':
                results.append(block.get('content'))
print('tool_use calls:', calls)
print('tool_result blocks:', len(results))
print('assistant text contained pong response:', text_with_token)
"
