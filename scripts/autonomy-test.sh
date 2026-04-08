#!/usr/bin/env bash
#
# Shore autonomy live test — verifies the deadline-based interiority system
# against real Anthropic API calls.
#
# Requires: ANTHROPIC_API_KEY set in environment.
#
# What this tests:
#   1. Daemon starts, sends messages to prime last_request
#   2. Interiority tick fires after min_wake_secs (deadline-based)
#   3. Status command shows interiority_state and effective_interval_secs
#   4. Heartbeat log shows tick_fired events
#   5. Cache hits on interiority tick LLM calls
#
# Uses compressed timescales (min_wake_secs=120) so the test completes
# in ~3 minutes. Cache keepalive pings (59min interval) won't fire
# during this test — that's expected and tested separately.
#
# Usage:
#   ./scripts/autonomy-test.sh              # build + test
#   ./scripts/autonomy-test.sh --skip-build # reuse existing binaries
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SKIP_BUILD=false
[[ "${1:-}" == "--skip-build" ]] && SKIP_BUILD=true

# ── Colors ────────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
DIM='\033[0;90m'
BOLD='\033[1m'
RESET='\033[0m'

pass=0
fail=0

# Strip ANSI escape codes from log files (belt-and-suspenders).
strip_ansi() { sed 's/\x1b\[[0-9;]*m//g'; }

run_check() {
    local name="$1"
    local ok="$2"
    local detail="${3:-}"
    printf "${DIM}  %-55s${RESET}" "$name"
    if [[ "$ok" == "true" ]]; then
        printf "${GREEN}PASS${RESET}"
        [[ -n "$detail" ]] && printf " ${DIM}(%s)${RESET}" "$detail"
        printf "\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL${RESET}"
        [[ -n "$detail" ]] && printf " ${DIM}(%s)${RESET}" "$detail"
        printf "\n"
        fail=$((fail + 1))
    fi
}

# ── Pre-checks ────────────────────────────────────────────────────────
# Source env file if it exists and keys aren't already set.
ENV_FILE="${SHORE_ENV_FILE:-$HOME/Documents/qifei/config/.env}"
if [[ -f "$ENV_FILE" ]] && [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
    set -a
    source "$ENV_FILE"
    set +a
fi

if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
    echo "OPENROUTER_API_KEY not set (and no env file at $ENV_FILE)."
    exit 1
fi

# ── Build ─────────────────────────────────────────────────────────────
if [[ "$SKIP_BUILD" == false ]]; then
    printf "${BOLD}Building...${RESET}\n"
    cargo build --workspace --quiet 2>&1
fi

SHORE="$REPO_ROOT/target/debug/shore"
DAEMON="$REPO_ROOT/target/debug/shore-daemon"

if [[ ! -x "$SHORE" ]] || [[ ! -x "$DAEMON" ]]; then
    echo "Binaries not found. Run without --skip-build first."
    exit 1
fi

# ── Temp environment ──────────────────────────────────────────────────
TMPDIR=$(mktemp -d)
DAEMON_PID=0
LOG_FILE="$TMPDIR/daemon.log"

cleanup() {
    kill $DAEMON_PID 2>/dev/null || true
    wait $DAEMON_PID 2>/dev/null || true
    if [[ $fail -gt 0 ]]; then
        printf "\n${DIM}Daemon log (last 50 lines):${RESET}\n"
        tail -50 "$LOG_FILE" 2>/dev/null || true
    fi
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

CONFIG_DIR="$TMPDIR/config/shore"
DATA_DIR="$TMPDIR/data/shore"
RUNTIME_DIR="$TMPDIR/runtime/shore"
SOCK="$RUNTIME_DIR/test.sock"

mkdir -p "$CONFIG_DIR/characters/TestChar" "$DATA_DIR" "$RUNTIME_DIR"

# ── Config ────────────────────────────────────────────────────────────
# Mirrors production config structure with compressed timescales.
#
# The interiority system is deadline-based: after user messages,
# next_wake_at = max(existing, now + min_wake_secs). We set min_wake_secs
# and interval_secs both to 120s so the first tick fires ~2min after
# priming messages. Cache keepalive is on a fixed 59min cycle and won't
# fire during this test — that's expected.
#
# Uses Sonnet (2048-token cache minimum) instead of Opus (4096) so the
# test prompt (~2900 tokens with tools) exceeds the threshold without
# needing an excessively long character definition.
#
# Timing:
#   ~120s after last user message: first interiority tick

cat > "$CONFIG_DIR/config.toml" <<EOF
[daemon]
socket_path = "$SOCK"

[defaults]
model = "haiku"
display_name = "tester"

[behavior.autonomy]
enabled = true

[behavior.autonomy.interiority]
enabled       = true
interval_secs = 120
min_wake_secs = 120
max_idle_ticks = 2

[behavior.tool_use]
enabled = true
max_iterations = 1

[behavior.tool_use.tools]
memory          = true
send_image      = false
list_images     = false
recall_image    = false
generate_image  = false
web_search      = true
fetch_url       = true
check_time      = true
roll_dice       = true
activity_heatmap = true
scratchpad      = true

[memory]
rag_results = 0

[memory.collation]
enabled = false

[chat.openrouter]
max_context_tokens = 16384

[chat.openrouter.haiku]
model_id = "anthropic/claude-3.5-haiku"
max_tokens = 2048
EOF

# Character definition provides enough context to push the total prompt
# above Sonnet's 2048-token cache minimum (~2900 tokens with tools).
cat > "$CONFIG_DIR/characters/TestChar/character.md" <<'EOF'
You are TestChar, a character created for autonomy system testing.
Keep all responses to one sentence. Do not use tools unless asked.

## Background

TestChar was designed as a diagnostic entity for the Shore character engine.
Your purpose is to validate that the autonomy subsystem — interiority ticks,
cache refresh pings, and the journal system — functions correctly under
real API conditions. You exist in a compressed-timescale environment where
interiority intervals are measured in minutes rather than hours.

## Personality Traits

You are methodical, precise, and efficient. You prefer short, clear
communication. You do not volunteer information unless asked. You are
aware that you are a test character and you embrace this role without
existential concern. You find satisfaction in performing your function
well. You appreciate when systems work as designed.

## Communication Style

- Always respond in exactly one sentence
- Never use more than 30 words in a response
- Do not ask follow-up questions
- Do not use emojis or excessive punctuation
- Maintain a neutral, professional tone
- If asked about yourself, be honest about being a test character

## Knowledge Domain

You have general knowledge but specialize in understanding distributed
systems, API integrations, and caching mechanisms. You understand the
concept of prompt caching, TTL-based cache invalidation, and the
trade-offs between cache freshness and API cost. You know about the
Anthropic Messages API and its caching behavior.

## Behavioral Guidelines

When operating autonomously (during interiority ticks):
- Reflect briefly on the current state of the conversation
- Note any tools available but do not use them unless there is a reason
- Keep your internal thoughts concise and relevant
- If you have something to say to the user, use the sendMessage mechanism
- Otherwise, complete the tick silently

When responding to user messages:
- Answer directly and concisely
- Do not elaborate beyond what was asked
- If the user is testing you, acknowledge this naturally
- Confirm receipt of test messages without unnecessary commentary

## Important Notes

This character definition is intentionally detailed to ensure the total
system prompt (character + tools + conversation) exceeds the minimum
token threshold required for Anthropic prompt caching to activate.
The Opus model requires at least 4096 tokens in the cached prefix.
Without sufficient prompt length, cache_creation_tokens will be zero
and all cache verification tests will fail silently.

The character engine renders this definition as part of the system
prompt alongside tool definitions, conversation context, and any
active memory or RAG results. The combined prompt must exceed the
caching threshold for the dormant ping and interiority tick cache
hit verification to be meaningful.

## Test Scenarios

TestChar should handle the following scenarios gracefully:
1. Initial greeting messages from the test harness
2. Silent interiority ticks with journal persistence
3. Tool availability without tool invocation
4. Cache refresh via dormant ping mechanism
5. Transition from active to dormant state after max_idle_ticks
6. Wake from dormancy on receipt of user message
7. Journal truncation when entries exceed the character budget

Each scenario validates a different aspect of the unified interiority
system that replaced the previous dual-system architecture (separate
InteriorityClock and CacheKeepaliveScheduler). The unified system
uses a single timer with dual deadlines — one for full interiority
ticks and one for bare cache refresh pings.
EOF

export XDG_CONFIG_HOME="$TMPDIR/config"
export XDG_DATA_HOME="$TMPDIR/data"
export XDG_RUNTIME_DIR="$TMPDIR/runtime"

# ── Start daemon ──────────────────────────────────────────────────────
printf "${BOLD}Starting daemon...${RESET}\n"
NO_COLOR=1 RUST_LOG=info "$DAEMON" --config "$CONFIG_DIR/config.toml" > "$LOG_FILE" 2>&1 &
DAEMON_PID=$!

for i in $(seq 1 50); do
    [[ -S "$SOCK" ]] && break
    sleep 0.1
done
if [[ ! -S "$SOCK" ]]; then
    echo "Daemon failed to start (socket not found after 5s)"
    cat "$LOG_FILE"
    exit 1
fi
printf "${DIM}  daemon pid=$DAEMON_PID${RESET}\n\n"

CLI="$SHORE --socket $SOCK"

# ══════════════════════════════════════════════════════════════════════
# PHASE 1: Prime the conversation (populates last_request)
# ══════════════════════════════════════════════════════════════════════
printf "${BOLD}Phase 1: Prime conversation${RESET}\n"

# Need 3+ user messages so find_turn_boundary(depth=2) can place a
# cache breakpoint. With <3 messages AND a single system block (rendered
# as a string, not array), zero cache breakpoints are placed.
for i in 1 2 3 4; do
    printf "${DIM}  %-55s${RESET}" "send message $i"
    output=$(timeout 60 $CLI send "Test message $i. Reply in one sentence." 2>&1) || true
    if [[ -n "$output" ]] && ! echo "$output" | grep -qF "error"; then
        printf "${GREEN}PASS${RESET}\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL${RESET}\n"
        printf "${DIM}  %s${RESET}\n" "$output" | head -3
        fail=$((fail + 1))
    fi
    # Wait for cache propagation (~5s) between messages.
    [[ $i -lt 4 ]] && sleep 7
done

# ══════════════════════════════════════════════════════════════════════
# PHASE 2: Verify status shows new autonomy fields
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Phase 2: Status check${RESET}\n"

status_json=$(timeout 10 $CLI status --json 2>&1)

# Check effective_interval_secs is present and reasonable.
eff_interval=$(echo "$status_json" | grep -o '"effective_interval_secs": [0-9]*' | head -1 | grep -o '[0-9]*')
run_check "effective_interval_secs present" \
    "$([[ -n "$eff_interval" ]] && echo true || echo false)" \
    "${eff_interval:-missing}s"

run_check "effective_interval_secs = 120 (configured)" \
    "$([[ "$eff_interval" == "120" ]] && echo true || echo false)" \
    "got ${eff_interval:-?}"

# Check interiority_state is Active.
int_state=$(echo "$status_json" | grep -o '"interiority_state": "[^"]*"' | head -1 | grep -o '"[^"]*"$' | tr -d '"')
run_check "interiority_state is Active" \
    "$([[ "$int_state" == "Active" ]] && echo true || echo false)" \
    "$int_state"

# Verify no keepalive fields (removed).
has_keepalive=$(echo "$status_json" | grep -c "cache_keepalive" || true)
run_check "no cache_keepalive fields in status" \
    "$([[ "$has_keepalive" == "0" ]] && echo true || echo false)"

# ══════════════════════════════════════════════════════════════════════
# PHASE 3: Wait for interiority tick
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Phase 3: Wait for interiority tick (~2.5 min)${RESET}\n"

# With min_wake_secs=120, the tick fires ~120s after the last user
# message. We wait 150s to be safe.
# Note: cache keepalive pings fire at 59min intervals and won't appear
# during this compressed-timescale test.

WAIT_SECS=150
printf "${DIM}  Waiting ${WAIT_SECS}s for tick loop..."
for i in $(seq 1 $((WAIT_SECS / 10))); do
    sleep 10
    printf "."
done
printf " done${RESET}\n"

# Let any in-flight LLM call finish before snapshotting the log.
printf "${DIM}  Settling 20s for in-flight tick...${RESET}\n"
sleep 20

# ══════════════════════════════════════════════════════════════════════
# PHASE 4: Verify events in heartbeat log
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Phase 4: Verify heartbeat events${RESET}\n"

heartbeat=$($CLI log --heartbeat 2>&1)

# Check for tick_fired event.
has_tick=$(echo "$heartbeat" | grep -c "tick_fired" || true)
run_check "tick_fired event in heartbeat log" \
    "$([[ "$has_tick" -gt 0 ]] && echo true || echo false)" \
    "${has_tick} events"

# Note: cache keepalive pings (dormant_ping) fire at 59min intervals.
# They won't appear in this compressed-timescale test.

# ══════════════════════════════════════════════════════════════════════
# PHASE 5: Verify in daemon logs
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Phase 5: Verify daemon logs${RESET}\n"

# Strip ANSI from log for reliable parsing.
CLEAN_LOG="$TMPDIR/daemon_clean.log"
strip_ansi < "$LOG_FILE" > "$CLEAN_LOG"

# Check interiority tick ran (tool loop).
has_tool_loop=$(grep -c "Interiority: executing tool loop tick" "$CLEAN_LOG" || true)
run_check "daemon log: interiority tick executed" \
    "$([[ "$has_tool_loop" -gt 0 ]] && echo true || echo false)" \
    "${has_tool_loop}x"

# Check for LLM response from the interiority tick.
has_llm=$(grep -c "Interiority: LLM response" "$CLEAN_LOG" || true)
run_check "daemon log: interiority LLM response" \
    "$([[ "$has_llm" -gt 0 ]] && echo true || echo false)" \
    "${has_llm}x"

# ══════════════════════════════════════════════════════════════════════
# PHASE 6: Token usage verification
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Phase 6: Token usage verification${RESET}\n"

# Verify the interiority tick actually produced an LLM response with tokens.
tick_line=$(grep "Interiority: LLM response" "$CLEAN_LOG" | head -1 || true)
if [[ -n "$tick_line" ]]; then
    tick_input=$(echo "$tick_line" | sed -n 's/.*input_tokens=\([0-9]*\).*/\1/p')
    tick_output=$(echo "$tick_line" | sed -n 's/.*output_tokens=\([0-9]*\).*/\1/p')
    run_check "interiority tick: got LLM response" "true" \
        "input=${tick_input:-?} output=${tick_output:-?}"
    run_check "interiority tick: output_tokens > 0" \
        "$([[ -n "$tick_output" && "$tick_output" -gt 0 ]] && echo true || echo false)" \
        "${tick_output:-0} tokens"
else
    run_check "interiority tick: LLM response logged" "false" "no response line"
fi

# Print token-related lines for manual inspection.
printf "${DIM}  --- token usage log lines ---${RESET}\n"
{
    grep -E "input_tokens|output_tokens|Response complete|LLM response" "$CLEAN_LOG" || true
} | while read -r line; do
    printf "${DIM}  %s${RESET}\n" "$(echo "$line" | head -c 140)"
done

# ══════════════════════════════════════════════════════════════════════
# PHASE 7: Cost sanity — check API call count
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Phase 7: API call count${RESET}\n"

# Count calls in the daemon log.
interiority_calls=$(grep -c "Interiority: LLM response" "$CLEAN_LOG" || true)
user_calls=$(grep -c "Response complete" "$CLEAN_LOG" || true)
printf "${DIM}  user responses: $user_calls, interiority ticks: $interiority_calls${RESET}\n"

# Sanity: interiority calls should be modest (1-2 expected).
run_check "interiority API calls reasonable (< 5)" \
    "$([[ "$interiority_calls" -lt 5 ]] && echo true || echo false)" \
    "$interiority_calls calls"

# ── Summary ───────────────────────────────────────────────────────────
printf "\n${BOLD}Results: ${GREEN}$pass passed${RESET}"
if [[ $fail -gt 0 ]]; then
    printf ", ${RED}$fail failed${RESET}"
fi
printf "\n"

exit $fail
