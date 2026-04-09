#!/usr/bin/env bash
#
# Test: Default cache config (no env var overrides) with recap present.
#
# Verifies that the hardcoded defaults (depth=[1,2] pinned=[-1]) work
# out of the box without any explicit configuration.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "defaults-recap"

# No cache config overrides — rely entirely on hardcoded defaults.
CACHE_DEPTH_TURNS=""
CACHE_PINNED_POSITION=""
REASONING_EFFORT="high"

# Seed a recap so the system prompt has multiple blocks.
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
