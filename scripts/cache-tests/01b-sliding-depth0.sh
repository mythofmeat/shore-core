#!/usr/bin/env bash
#
# Test: Single sliding cache breakpoint at depth=0.
#
# depth=0 means the breakpoint is at the very end of messages — no
# sliding, just caching the entire prefix every time.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "sliding-depth0"

CACHE_DEPTH_TURNS="[0]"
CACHE_PINNED_POSITION=""
REASONING_EFFORT="high"

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
