#!/usr/bin/env bash
#
# Test: pinned=[0] depth=[1,2] WITHOUT recap. (reconfirm test 06)
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "pinned0-no-recap"

CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION="[0]"
REASONING_EFFORT="high"

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
