#!/usr/bin/env bash
#
# Test: Realistic config — pinned=[-1] with recap.md seeded, depth=[1,2].
#
# This matches how the user would actually configure their setup:
# a recap (adding a second system block after the character definition),
# with pinned=[-1] targeting the second-to-last system block, plus
# two sliding message breakpoints at depth 1 and 2.
#
# System blocks produced by assemble_prompt:
#   [0] rendered system.md template
#   [1] character definition
#   [2] recap (from memory/recap.md)
#
# pinned=[-1] → second-to-last → index 1 (character definition)
# depth=[1,2] → two sliding message breakpoints
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "pinned-neg1-recap"

# Config: pinned on second-to-last system block + two sliding breakpoints.
CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION="[-1]"
REASONING_EFFORT="high"

# Seed a recap so there are multiple system blocks.
mkdir -p "$DATA_DIR/$CHARACTER_NAME/memory"
cat > "$DATA_DIR/$CHARACTER_NAME/memory/recap.md" << 'RECAP'
The conversation has covered a range of topics so far. The user asked about
prompt caching and how it works with the Anthropic API. They discussed the
economics of cache writes versus reads, and explored different breakpoint
configurations. The character explained the difference between sliding and
pinned breakpoints, and how system prompt anchoring affects cache stability.
The user seemed particularly interested in matching SillyTavern's caching
approach, which uses both system-level and message-level breakpoints.
RECAP

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
