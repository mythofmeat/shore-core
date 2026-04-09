#!/usr/bin/env bash
#
# Test: depth=[1] pinned=[-1] → always exactly 2 breakpoints.
# Hypothesis: cache invalidation is caused by breakpoint COUNT changing,
# not by breakpoint position moving.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "stable-count"

CACHE_DEPTH_TURNS="[1]"
CACHE_PINNED_POSITION="[-1]"
REASONING_EFFORT="high"
OPENROUTER_PROVIDER=""

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
done

harness_pass
