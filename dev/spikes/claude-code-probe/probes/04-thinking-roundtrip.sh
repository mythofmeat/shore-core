#!/usr/bin/env bash
# Probe 4: extended-thinking blocks across stream-json turns.
#
# Step A: ask a thinking-capable model a hard-ish question. Capture
# any thinking blocks in the response.
# Step B: open a fresh invocation, feed back a conversation that
# includes the prior assistant turn *with* the thinking block, then
# ask a follow-up. See if the CLI accepts the input or rejects it.
set -euo pipefail
source "$(dirname "$0")/_common.sh"

THINK_MODEL="${THINK_MODEL:-claude-opus-4-5}"

OUT_A="$RESULTS_DIR/04a-thinking-capture.jsonl"
OUT_B="$RESULTS_DIR/04b-thinking-replay.jsonl"
OUT_B_INPUT="$RESULTS_DIR/04b-thinking-replay.input.ndjson"

# Step A: only run if not already captured.
if [[ ! -s "$OUT_A" ]]; then
    banner "Probe 04a: capture thinking block ($THINK_MODEL)"
    claude --print \
        --output-format stream-json \
        --input-format stream-json \
        --verbose \
        --no-session-persistence \
        --strict-mcp-config \
        --model "$THINK_MODEL" \
        --tools "" \
        --effort high \
        --system-prompt "Think carefully step by step before answering. Be terse in the final answer." \
        < <(echo '{"type":"user","message":{"role":"user","content":"What is the smallest positive integer n for which n! has more than 100 trailing zeros? Just the number."}}') \
        > "$OUT_A" || true
fi

banner "Probe 04a: thinking-capture summary"
python3 - "$OUT_A" <<'PY'
import json, sys
path = sys.argv[1]
thinking_blocks, text_blocks, others = [], [], []
for line in open(path):
    line=line.strip()
    if not line: continue
    try: ev = json.loads(line)
    except json.JSONDecodeError: continue
    if ev.get('type') == 'assistant':
        for block in ev.get('message',{}).get('content',[]):
            t = block.get('type')
            if t == 'thinking': thinking_blocks.append(block)
            elif t == 'text': text_blocks.append(block)
            else: others.append(t)
print(f'thinking blocks: {len(thinking_blocks)}')
print(f'text blocks: {len(text_blocks)}')
print(f'other block types: {others}')
if thinking_blocks:
    sample = thinking_blocks[0]
    print('thinking-block keys:', sorted(sample.keys()))
    print('signature length:', len(sample.get('signature','')))
PY

# Step B: build a 3-turn input replaying the thinking block.
banner "Probe 04b: build replay input"
python3 - "$OUT_A" "$OUT_B_INPUT" <<'PY'
import json, sys
src, dst = sys.argv[1:3]
assistant_blocks = []
for line in open(src):
    line=line.strip()
    if not line: continue
    try: ev = json.loads(line)
    except json.JSONDecodeError: continue
    if ev.get('type') == 'assistant':
        for block in ev.get('message',{}).get('content',[]):
            if block.get('type') in ('thinking','text','redacted_thinking'):
                assistant_blocks.append(block)
if not assistant_blocks:
    print('(no assistant blocks captured; cannot run replay)')
    sys.exit(2)
frames = [
    {"type":"user","message":{"role":"user","content":"What is the smallest positive integer n for which n! has more than 100 trailing zeros? Just the number."}},
    {"type":"assistant","message":{"role":"assistant","content":assistant_blocks}},
    {"type":"user","message":{"role":"user","content":"Now confirm: does that number divide 1000?"}},
]
with open(dst,'w') as f:
    for frame in frames: f.write(json.dumps(frame)+"\n")
print(f'wrote {len(frames)} frames to {dst}')
PY

banner "Probe 04b: run replay"
claude --print \
    --output-format stream-json \
    --input-format stream-json \
    --verbose \
    --no-session-persistence \
    --strict-mcp-config \
    --model "$THINK_MODEL" \
    --tools "" \
    --effort high \
    --system-prompt "Think carefully step by step before answering. Be terse in the final answer." \
    < "$OUT_B_INPUT" \
    > "$OUT_B" 2>&1 || true

echo "--- replay summary ---"
python3 - "$OUT_B" <<'PY'
import json, sys
path = sys.argv[1]
ok = False
err = None
text = []
for line in open(path):
    line=line.strip()
    if not line: continue
    try: ev = json.loads(line)
    except json.JSONDecodeError:
        # Probably an error string; show it
        print('NON-JSON:', line[:200]); continue
    t = ev.get('type')
    if t == 'result':
        ok = not ev.get('is_error', False)
        print('result subtype:', ev.get('subtype'), 'is_error:', ev.get('is_error'))
        print('result text:', repr((ev.get('result') or '')[:300]))
    elif t == 'assistant':
        for block in ev.get('message',{}).get('content',[]):
            if block.get('type') == 'text':
                text.append(block.get('text',''))
print('replay accepted with thinking block:', ok)
PY
