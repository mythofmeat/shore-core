#!/usr/bin/env bash
#
# Test: Single sliding breakpoint with NO thinking/reasoning.
#
# This isolates whether the thinking parameter affects cache behavior.
# Expected: Same as 01 — first writes, rest reads.
# If this fails but 01 passes, thinking config is a cache factor.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "no-thinking"

# Config: one sliding breakpoint, no reasoning.
CACHE_DEPTH_TURNS="[2]"
CACHE_PINNED_POSITION=""
REASONING_EFFORT=""

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
