#!/usr/bin/env bash
#
# Test: depth=[1,2] pinned=[-1] with warm-up turns so the breakpoint
# COUNT is stable at 3 from the start.
#
# Hypothesis: the cache bust at turn 3 in test 13 was caused by the
# breakpoint count changing from 2→3, not by breakpoint movement.
# If we start with enough messages that depth=[1,2] resolves to 2
# distinct indices from the beginning, the count stays at 3 throughout
# and caching should be stable.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "stable-count-3"

CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION="[-1]"
REASONING_EFFORT="high"
OPENROUTER_PROVIDER=""

mkdir -p "$DATA_DIR/$CHARACTER_NAME/memory"
cat > "$DATA_DIR/$CHARACTER_NAME/memory/recap.md" << 'RECAP'
The conversation has covered a range of topics so far. The user asked about
prompt caching and how it works with the Anthropic API. They discussed the
economics of cache writes versus reads, and explored different breakpoint
configurations. The character explained the difference between sliding and
pinned breakpoints, and how system prompt anchoring affects cache stability.
RECAP

harness_start

# Warm-up: 2 turns to ensure depth=[1,2] resolves to 2 distinct
# message indices from the very first "real" turn onward.
# After these 2 turns we have 4 messages (u,a,u,a) + the next user = 5.
# depth=1 → find user at -2 from end → index 3 (assistant)
# depth=2 → find user at -3 from end → index 1 (assistant)
# Both resolve distinctly → 2 message breakpoints + 1 system = 3 total.
echo -e "${CYAN}[stable-count-3]${NC} sending warm-up turns..."
send_msg "Warm-up turn 1. Hello."
send_msg "Warm-up turn 2. Still here."

# Reset threshold after warm-up so the real test starts clean.
_MSG_INDEX=0
_FIRST_WRITE=0
_WRITE_THRESHOLD=0

echo -e "${CYAN}[stable-count-3]${NC} warm-up complete, starting real test..."

for i in $(seq 1 10); do
    send_msg "Cache test turn $i. What is $((RANDOM % 100)) plus $((RANDOM % 100))?"
done

harness_pass
