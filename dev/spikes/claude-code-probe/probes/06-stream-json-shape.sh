#!/usr/bin/env bash
# Probe 6: capture full stream-json output for a vanilla turn so we
# have a concrete schema to design the Rust parser against.
set -euo pipefail
source "$(dirname "$0")/_common.sh"

OUT="$RESULTS_DIR/06-stream-json-shape.jsonl"

banner "Probe 06: stream-json shape capture"

claude --print \
    --output-format stream-json \
    --input-format stream-json \
    --verbose \
    --no-session-persistence \
    --model "$PROBE_MODEL" \
    --tools "" \
    --system-prompt "Reply in exactly two short sentences." \
    < <(echo '{"type":"user","message":{"role":"user","content":"Say hi."}}') \
    > "$OUT" || true

echo "Wrote $OUT"
echo "--- distinct top-level event shapes ---"
python3 -c "
import json
shapes = {}
for line in open('$OUT'):
    line=line.strip()
    if not line: continue
    try: ev = json.loads(line)
    except json.JSONDecodeError: continue
    t = ev.get('type','?')
    keys = tuple(sorted(ev.keys()))
    shapes.setdefault((t,keys), 0)
    shapes[(t,keys)] += 1
for (t,keys),n in shapes.items():
    print(f'  type={t} keys={keys} count={n}')
"
