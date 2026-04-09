#!/usr/bin/env bash
#
# Test: Single sliding cache breakpoint at depth=1.
#
# depth=1 means the breakpoint sits one user turn back from the end.
# It slides every turn, so we expect a small write each time as the
# breakpoint extends, but the system prefix should always be read.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "sliding-depth1"

CACHE_DEPTH_TURNS="[1]"
CACHE_PINNED_POSITION=""
REASONING_EFFORT="high"

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
