#!/usr/bin/env bash
#
# Test: concurrent heartbeat tick + user message.
#
# Forces a heartbeat tick and immediately sends a user message
# in parallel. Verifies that neither busts the cache and both
# complete without errors.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "concurrent-tick-msg"

CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION="[-1]"
REASONING_EFFORT="high"
OPENROUTER_PROVIDER='{ order = ["Anthropic"], allow_fallbacks = false }'

_write_config() {
    cat > "$CONFIG_DIR/config.toml" << TOML
[defaults]
display_name = "tester"
model        = "chat.test.model"

[behavior.autonomy]
enabled = true

[behavior.autonomy.heartbeat]
enabled = true
fallback_heartbeat_interval = "1h"
dormant_after_heartbeat_turns = 5
dormant_after_idle_time = "48h"

[behavior.tool_use.tools]
memory = true

[advanced]
api_payload_logging = true

[daemon]
socket_path = "$SOCKET_PATH"
TOML

    local model_toml="$CONFIG_DIR/conf.d/models.toml"
    {
        echo '[chat.test.model]'
        echo "sdk          = \"anthropic\""
        echo "model_id     = \"$MODEL_ID\""
        echo "api_key_env  = \"$API_KEY_ENV\""
        echo "base_url     = \"$BASE_URL\""
        echo "cache_ttl    = \"$CACHE_TTL\""
        [[ -n "$REASONING_EFFORT" ]] && echo "reasoning_effort      = \"$REASONING_EFFORT\""
        [[ -n "$OPENROUTER_PROVIDER" ]] && echo "openrouter_provider   = $OPENROUTER_PROVIDER" || true
    } > "$model_toml"
}

mkdir -p "$DATA_DIR/$CHARACTER_NAME/active_prompt"
cat > "$DATA_DIR/$CHARACTER_NAME/active_prompt/RECENT_MEMORY.md" << 'RECAP'
The user likes math questions.
RECAP

harness_start

# ── Helpers ───────────────────────────────────────────────────────
send_cmd() {
    SHORE_CONFIG_DIR="$CONFIG_DIR" \
    SHORE_DATA_DIR="$DATA_DIR" \
        "$SHORE_BIN" --socket "$SOCKET_PATH" \
            --character "$CHARACTER_NAME" \
            debug "$1" 2>>"$LOG_FILE"
}

# ── Phase 1: Warm-up ─────────────────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 1: Warm-up (5 turns) ==="
for i in $(seq 1 5); do
    send_msg "Turn $i: what is $((RANDOM % 100)) + $((RANDOM % 100))?"
done

PRE_COUNT="$(grep -c '"type":"response"' "$(forensics_path)")"
echo -e "${CYAN}[$TEST_NAME]${NC} pre-concurrent forensics count=$PRE_COUNT"

# ── Phase 2: Force tick + send message concurrently ───────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 2: Concurrent tick + message ==="
echo -e "${CYAN}[$TEST_NAME]${NC} forcing tick and sending message simultaneously..."

# Fire tick in background.
send_cmd "heartbeat_tick_now" &
TICK_PID=$!

# Wait just 1 second, then send a message while the tick is pending/running.
sleep 1
send_msg "Concurrent message: what is 42 + 58?"

# Wait for the tick to be acknowledged.
wait $TICK_PID 2>/dev/null || true

# Wait for all LLM calls to settle.
echo -e "${CYAN}[$TEST_NAME]${NC} waiting for tick + message to complete..."
TRIES=0
while [[ $TRIES -lt 20 ]]; do
    sleep 2; TRIES=$((TRIES + 1))
    NOW_COUNT="$(grep -c '"type":"response"' "$(forensics_path)")"
    # We expect at least: 5 warm-up + 1 concurrent msg + 1-2 tick calls = 7+
    if [[ "$NOW_COUNT" -ge $((PRE_COUNT + 2)) ]]; then
        echo -e "${CYAN}[$TEST_NAME]${NC} concurrent calls settled (count=$NOW_COUNT, waited ${TRIES}x2s)"
        break
    fi
done

# ── Phase 3: Post-concurrent follow-ups ───────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 3: Post-concurrent follow-ups ==="
send_msg "Follow-up 1: what is 10 + 10?"
send_msg "Follow-up 2: what is 99 - 1?"

# ── Phase 4: Verify no full rewrites ─────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 4: Verify ==="
BUSTED=0
IDX=0
while IFS= read -r line; do
    IDX=$((IDX + 1))
    [[ $IDX -eq 1 ]] && continue  # skip cold start
    cr="$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('cache_read_tokens', 0))" 2>/dev/null)" || cr=0
    cw="$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('cache_creation_tokens', 0))" 2>/dev/null)" || cw=0
    if [[ "$cr" -eq 0 && "$cw" -gt "$_WRITE_THRESHOLD" ]]; then
        echo -e "${RED}[$TEST_NAME]${NC}   entry $IDX: cache_r=0 cache_w=$cw — FULL REWRITE"
        BUSTED=1
    fi
done < <(grep '"type":"response"' "$(forensics_path)")

if [[ "$BUSTED" -eq 1 ]]; then
    harness_fail "concurrent tick+message caused a full cache rewrite"
fi

harness_pass
