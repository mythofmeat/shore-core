#!/usr/bin/env bash
# Probe 1: --system-prompt overrides the default.
#
# We pass a deliberately distinctive system prompt and ask a question
# whose answer would differ between the default Claude Code agent
# (which would talk about coding and tools) and our character (which
# is told to act like a haiku-only librarian). If the response is a
# haiku, the override worked.
set -euo pipefail
source "$(dirname "$0")/_common.sh"

OUT="$RESULTS_DIR/01-system-prompt.json"

banner "Probe 01: --system-prompt full override"

claude --print \
    --output-format json \
    --no-session-persistence \
    --model "$PROBE_MODEL" \
    --tools "" \
    --system-prompt "You are an austere librarian who only ever responds in 5-7-5 haiku. You will not write code, will not use tools, will not break form. Respond in exactly one haiku, no preamble." \
    "What is the meaning of recursion?" \
    > "$OUT"

echo "Wrote $OUT"
echo "--- Excerpted result text ---"
python3 -c "import json,sys; d=json.load(open('$OUT')); print(d.get('result',d))"
