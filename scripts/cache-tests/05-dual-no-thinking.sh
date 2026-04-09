#!/usr/bin/env bash
#
# Test: Dual breakpoints with NO thinking/reasoning.
#
# This is the specific combination that was failing before.
# If 03 passes (dual + thinking) but this fails, thinking is required
# for multi-breakpoint caching to work on OpenRouter.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "dual-no-thinking"

# Config: system + message breakpoints, no reasoning.
CACHE_DEPTH_TURNS="[2]"
CACHE_PINNED_POSITION="[0]"
REASONING_EFFORT=""

harness_start

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
