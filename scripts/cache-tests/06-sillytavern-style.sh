#!/usr/bin/env bash
#
# Test: SillyTavern-style — system pinned + sliding depth 1 and 2.
#
# Matches SillyTavern's full approach: system prompt breakpoint on last
# system block, plus two message breakpoints at depth 1 and depth 2
# (equivalent to SillyTavern's cachingAtDepth and cachingAtDepth+2).
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "sillytavern-style"

# Config: system breakpoint + two sliding message breakpoints.
CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION="[0]"
REASONING_EFFORT="high"

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
