#!/usr/bin/env bash
#
# Test: Single sliding cache breakpoint (depth=2).
#
# Expected: First message writes to cache. All subsequent messages read
# from cache with zero writes (small writes for new content are ok, but
# a full rewrite at the system prompt size means the prefix changed).
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "single-sliding"

# Config: one sliding breakpoint, no system breakpoint.
CACHE_DEPTH_TURNS="[2]"
CACHE_PINNED_POSITION=""
REASONING_EFFORT="high"

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
