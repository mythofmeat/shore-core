#!/usr/bin/env bash
#
# Test: Two sliding breakpoints at depth 1 and 2 (SillyTavern-style).
#
# SillyTavern places breakpoints at cachingAtDepth and cachingAtDepth+2,
# which in role-switch terms is equivalent to our depth 1 and depth 2.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "sliding-depth1-and-2"

CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION=""
REASONING_EFFORT="high"

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
