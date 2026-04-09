#!/usr/bin/env bash
#
# Test: Dual breakpoints — system pinned + message sliding.
#
# This is the configuration we WANT to work but was previously failing.
# Expected: First message writes. Subsequent messages should read from
# cache. If this test fails but 01 and 02 pass, the issue is with
# multiple breakpoints specifically.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "dual-breakpoints"

# Config: system breakpoint + sliding message breakpoint.
CACHE_DEPTH_TURNS="[2]"
CACHE_PINNED_POSITION="[0]"
REASONING_EFFORT="high"

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
