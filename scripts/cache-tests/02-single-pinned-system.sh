#!/usr/bin/env bash
#
# Test: Single pinned system breakpoint (position=0, last system block).
#
# Expected: First message writes to cache. All subsequent messages should
# read the system prefix from cache. Small writes for new message content
# beyond the breakpoint are expected, but system-sized writes are failures.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "single-pinned-system"

# Config: one system breakpoint, no message breakpoint.
CACHE_DEPTH_TURNS=""
CACHE_PINNED_POSITION="[0]"
REASONING_EFFORT="high"

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
