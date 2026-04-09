#!/usr/bin/env bash
#
# Test: cache stability through an interiority tick.
#
# Sequence:
#   1. Send 3 messages (warm up cache, establish baseline)
#   2. Force an interiority tick via `shore debug force-tick`
#   3. Wait for the tick to complete (~15s)
#   4. Send 2 more messages
#   5. Check that cache_r never dropped to 0 after the cold start
#
# Uses provider pin to Anthropic (the known-stable config).
# Autonomy must be enabled for the tick to fire.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "interiority-tick"

CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION="[-1]"
REASONING_EFFORT="high"
OPENROUTER_PROVIDER='{ order = ["Anthropic"], allow_fallbacks = false }'

# Override _write_config to enable autonomy with short intervals.
_write_config() {
    cat > "$CONFIG_DIR/config.toml" << TOML
[defaults]
display_name = "tester"
model        = "chat.test.model"

[behavior.autonomy]
enabled = true

[behavior.autonomy.interiority]
enabled = true
fallback_interiority_interval = "1h"
dormant_after_interiority_turns = 5
dormant_after_idle_time = "48h"

[behavior.tool_use.tools]
memory = false

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

# Write a recap so the interiority prompt has context.
mkdir -p "$DATA_DIR/$CHARACTER_NAME/memory"
cat > "$DATA_DIR/$CHARACTER_NAME/memory/recap.md" << 'RECAP'
The conversation has covered a range of topics so far. The user asked about
prompt caching and how it works with the Anthropic API. They discussed the
economics of cache writes versus reads, and explored different breakpoint
configurations. The character explained the difference between sliding and
pinned breakpoints, and how system prompt anchoring affects cache stability.
RECAP

harness_start

# ── Helper: send a command ────────────────────────────────────────
send_cmd() {
    local cmd="$1"
    echo -e "${CYAN}[$TEST_NAME]${NC} cmd: $cmd"
    SHORE_CONFIG_DIR="$CONFIG_DIR" \
    SHORE_DATA_DIR="$DATA_DIR" \
        "$SHORE_BIN" --socket "$SOCKET_PATH" \
            --character "$CHARACTER_NAME" \
            debug "$cmd" 2>>"$LOG_FILE"
}

# ── Helper: count forensics lines ─────────────────────────────────
forensics_count() {
    local path
    path="$(forensics_path)"
    [[ -f "$path" ]] || { echo 0; return; }
    grep -c '"type":"response"' "$path" 2>/dev/null || echo 0
}

# ── Helper: wait for interiority tick to appear in forensics ──────
wait_for_tick() {
    local path
    path="$(forensics_path)"
    local before
    before="$(forensics_count)"
    echo -e "${CYAN}[$TEST_NAME]${NC} waiting for interiority tick (forensics count=$before)..."

    local tries=0
    while [[ $tries -lt 30 ]]; do
        sleep 2
        tries=$((tries + 1))
        local now
        now="$(forensics_count)"
        if [[ "$now" -gt "$before" ]]; then
            # Check if the new entry is from an interiority call.
            local last
            last="$(tail -5 "$path" | grep '"type":"response"' | tail -1)"
            echo -e "${CYAN}[$TEST_NAME]${NC} new forensics entry detected (count=$now, waited ${tries}x2s)"

            local cache_r cache_w
            cache_r="$(echo "$last" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('cache_read_tokens', 0))" 2>/dev/null)" || cache_r=0
            cache_w="$(echo "$last" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('cache_creation_tokens', 0))" 2>/dev/null)" || cache_w=0
            echo -e "${CYAN}[$TEST_NAME]${NC}   tick: cache_r=$cache_r cache_w=$cache_w"
            return 0
        fi
    done
    echo -e "${YELLOW}[$TEST_NAME]${NC} timeout waiting for interiority tick"
    return 1
}

# ── Phase 1: Warm-up ──────────────────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 1: Warm-up ==="
send_msg "Hello! What is 2 + 2?"
send_msg "What is 10 * 5?"
send_msg "What is 100 / 4?"

# ── Phase 2: Force interiority tick ───────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 2: Force interiority tick ==="
send_cmd "force-tick"
wait_for_tick || harness_fail "interiority tick did not fire"

# ── Phase 3: Post-tick follow-up ──────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 3: Post-tick follow-up ==="
send_msg "Hey, what is 3 + 3?"
send_msg "And 7 * 7?"

harness_pass
