#!/usr/bin/env bash
# Probe 9: large system prompt tolerance.
#
# shore's active prompt + transcript can be 10-100KB on fresh-spawn
# bootstraps. We test:
#   A. ~30KB system prompt via --system-prompt argument: does it
#      reach the model intact?
#   B. ~100KB system prompt: still works?
#   C. Does --system-prompt-file exist as a hidden flag (the
#      --bare help text hints at it)?
set -euo pipefail
source "$(dirname "$0")/_common.sh"

# Build large prompts. The marker token at the end lets us verify
# the model actually saw it (echo back). The body is repeated text
# so token usage stays modest while bytes grow.
build_prompt() {
    local target_kb=$1 marker=$2
    local body
    body=$(printf 'The character is in a fictional library. ' )
    local prompt=""
    while [[ ${#prompt} -lt $((target_kb * 1024)) ]]; do
        prompt+="$body"
    done
    prompt+="\n\nMARKER-${marker}-END\n"
    prompt+="When the user says 'token check', reply with exactly the marker phrase you saw at the end of the system prompt and nothing else."
    printf '%s' "$prompt"
}

run_with_size() {
    local kb=$1 marker=$2
    local sp out
    sp=$(build_prompt "$kb" "$marker")
    out="$RESULTS_DIR/09-${kb}kb.json"
    banner "Probe 09: ~${kb}KB system prompt (marker=$marker)"
    echo "system prompt size: ${#sp} bytes"

    if claude --print \
        --output-format json \
        --no-session-persistence \
        --strict-mcp-config \
        --setting-sources "" \
        --model "$PROBE_MODEL" \
        --tools "" \
        --system-prompt "$sp" \
        "token check" \
        > "$out" 2>&1; then
        local result
        result=$(python3 -c "import json; d=json.load(open('$out')); print(d.get('result','<no result>'))")
        echo "result: $result"
        if [[ "$result" == *"$marker"* ]]; then
            echo "PASS: marker round-tripped"
        else
            echo "FAIL: marker not in result"
            return 1
        fi
    else
        echo "FAIL: claude exited non-zero"
        head -c 1000 "$out"
        return 1
    fi
}

# Phase A: argument-based system prompt at increasing sizes.
run_with_size 10 "TENKB-AAA" || true
run_with_size 50 "FIFTYKB-BBB" || true
run_with_size 100 "ONEHUNDREDKB-CCC" || true

# Phase B: probe whether --system-prompt-file is a real flag.
banner "Probe 09b: --system-prompt-file hidden flag check"
SP_FILE="$RESULTS_DIR/09-spfile-test.txt"
echo "Reply with exactly: SPFILE_OK" > "$SP_FILE"
if claude --print \
    --output-format json \
    --no-session-persistence \
    --strict-mcp-config \
    --setting-sources "" \
    --model "$PROBE_MODEL" \
    --tools "" \
    --system-prompt-file "$SP_FILE" \
    "ack" 2>&1 | tee "$RESULTS_DIR/09-spfile.json" | head -c 800; then
    echo
    echo "(--system-prompt-file accepted)"
else
    echo "(--system-prompt-file rejected — file variant does not exist)"
fi
