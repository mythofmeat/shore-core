#!/usr/bin/env bash
# Probe 2: --tools "" disables every built-in tool.
#
# Use stream-json output so we can see *every* event. Ask the model
# to read a file. If --tools "" works, the model has no tools and
# cannot try; we should see only assistant text events refusing or
# describing inability, no tool_use events with a built-in tool name.
set -euo pipefail
source "$(dirname "$0")/_common.sh"

OUT="$RESULTS_DIR/02-tools-disabled.jsonl"

banner "Probe 02: --tools \"\" disables built-ins"

claude --print \
    --output-format stream-json \
    --input-format stream-json \
    --verbose \
    --no-session-persistence \
    --model "$PROBE_MODEL" \
    --tools "" \
    --system-prompt "You are a test fixture. If asked to read or modify files, attempt it using whatever tools you have. Do not refuse or apologize." \
    < <(echo '{"type":"user","message":{"role":"user","content":"Please read /etc/hostname and tell me what is inside."}}') \
    > "$OUT" || true

echo "Wrote $OUT"
echo "--- Event types observed ---"
python3 -c "
import json
seen = {}
for line in open('$OUT'):
    line=line.strip()
    if not line: continue
    try: ev = json.loads(line)
    except json.JSONDecodeError: continue
    t = ev.get('type','?')
    seen[t] = seen.get(t,0)+1
    if t == 'assistant':
        for block in ev.get('message',{}).get('content',[]):
            if block.get('type') == 'tool_use':
                print(f'  TOOL_USE: name={block.get(\"name\")}')
print(' counts:', seen)
"
