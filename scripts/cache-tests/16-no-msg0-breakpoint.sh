#!/usr/bin/env bash
#
# Test: depth=[1,2] pinned=[-1] with the msg[0] breakpoint filter.
#
# Hypothesis: Shore's cache instability through OpenRouter is caused by
# cache_control annotations appearing on the first user message during
# early turns (fallback in find_turn_boundary), then disappearing once
# the conversation has enough turns for depth resolution. This changes
# the serialized payload of the first non-system message, which
# OpenRouter uses in its sticky routing hash → provider switch → full
# cache miss.
#
# SillyTavern never places breakpoints on early messages (their sliding
# breakpoints count role switches from the end). This test verifies
# that filtering msg[0] breakpoints gives SillyTavern-like stability.
#
# A/B comparison: run this alongside test 08 (same config, no filter).
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "no-msg0-bp"

CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION="[-1]"
REASONING_EFFORT="high"

mkdir -p "$DATA_DIR/$CHARACTER_NAME/memory"
cat > "$DATA_DIR/$CHARACTER_NAME/memory/recap.md" << 'RECAP'
The conversation has covered a range of topics so far. The user asked about
prompt caching and how it works with the Anthropic API. They discussed the
economics of cache writes versus reads, and explored different breakpoint
configurations. The character explained the difference between sliding and
pinned breakpoints, and how system prompt anchoring affects cache stability.
RECAP

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
    # Delay between turns: avoids Anthropic's ~5s cache propagation window
    # and gives OpenRouter routing time to stabilize.
    if [[ $i -lt 10 ]]; then
        sleep 4
    fi
done

harness_pass
