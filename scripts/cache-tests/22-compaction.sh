#!/usr/bin/env bash
#
# Test: cache behavior through compaction.
#
# Compaction replaces conversation messages with a condensed summary.
# The system prompt + tools should remain cached (cache_r > 0), but
# the message-level cache will be invalidated (new message content).
#
# Sequence:
#   1. Send 10 messages (warm up, build cache)
#   2. Trigger compaction via `shore memory compact`
#   3. Send 3 more messages
#   4. Verify cache_r > 0 on post-compaction messages (system still cached)
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "compaction"

CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION="[-1]"
REASONING_EFFORT="high"
OPENROUTER_PROVIDER='{ order = ["Anthropic"], allow_fallbacks = false }'

_write_config() {
    cat > "$CONFIG_DIR/config.toml" << TOML
[defaults]
display_name = "tester"
model        = "chat.test.model"
embedding    = "openrouter:qwen/qwen3-embedding-8b"

[behavior.autonomy]
enabled = true

[behavior.autonomy.heartbeat]
enabled = true
fallback_heartbeat_interval = "1h"

[memory.compaction]
enabled = true
min_turns = 4
max_turns = 6
idle_trigger = "5s"

[behavior.tool_use.tools]
memory = true

[advanced]
api_payload_logging = true

[daemon]
addr = "$LISTEN_ADDR"
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
        echo ''
        echo '[providers.openrouter]'
        echo 'base_url    = "https://openrouter.ai/api/v1"'
        echo 'api_key_env = "OPENROUTER_SHORE_EMBEDDING"'
        echo ''
        echo '[embedding."openrouter:qwen/qwen3-embedding-8b"]'
        echo 'dimensions  = 4096'
    } > "$model_toml"
}

# Append embedding API key from qifei config if not in the harness .env.
if ! grep -q 'OPENROUTER_SHORE_EMBEDDING' "$CONFIG_DIR/.env" 2>/dev/null; then
    if [[ -f "$HOME/Documents/qifei/config/.env" ]]; then
        grep 'OPENROUTER_SHORE_EMBEDDING' "$HOME/Documents/qifei/config/.env" >> "$CONFIG_DIR/.env"
    fi
fi

mkdir -p "$CONFIG_DIR/characters/$CHARACTER_NAME/workspace/memory"
cat > "$CONFIG_DIR/characters/$CHARACTER_NAME/workspace/memory/MEMORY.md" << 'RECAP'
# Memory Index

The user enjoys asking simple math questions.
RECAP

harness_start

# Override the inline check function — this test validates post-compaction
# cache behavior, not per-turn breakpoint stability.
_check_latest_response() { :; }

# ── Helpers ───────────────────────────────────────────────────────
send_shore_cmd() {
    local subcmd="$1"
    shift
    echo -e "${CYAN}[$TEST_NAME]${NC} shore $subcmd $*"
    SHORE_CONFIG_DIR="$CONFIG_DIR" \
    SHORE_DATA_DIR="$DATA_DIR" \
        "$SHORE_BIN" --addr "$DAEMON_ADDR" \
            --character "$CHARACTER_NAME" \
            $subcmd "$@" 2>>"$LOG_FILE" || true
}

# ── Phase 1: Build conversation ───────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 1: Build conversation (12 turns) ==="
for i in $(seq 1 12); do
    send_msg "Turn $i: what is $((RANDOM % 100)) + $((RANDOM % 100))?"
done

# Record pre-compaction baseline.
PRE_COMPACT_LINE="$(grep '"type":"response"' "$(forensics_path)" | tail -1)"
PRE_COMPACT_R="$(echo "$PRE_COMPACT_LINE" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('cache_read_tokens', 0))" 2>/dev/null)" || PRE_COMPACT_R=0
echo -e "${CYAN}[$TEST_NAME]${NC} pre-compaction cache_r=$PRE_COMPACT_R"

# ── Phase 2: Trigger compaction ───────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 2: Compact ==="
PRE_COMPACT_COUNT="$(grep -c '"type":"response"' "$(forensics_path)")"
echo -e "${CYAN}[$TEST_NAME]${NC} running: shore memory compact"
COMPACT_OUT="$(SHORE_CONFIG_DIR="$CONFIG_DIR" SHORE_DATA_DIR="$DATA_DIR" \
    "$SHORE_BIN" --addr "$DAEMON_ADDR" --character "$CHARACTER_NAME" \
    memory compact 2>&1)" || true
echo -e "${CYAN}[$TEST_NAME]${NC} compact output: $COMPACT_OUT"

# Verify compaction actually ran — look for the current markdown-memory output.
if ! echo "$COMPACT_OUT" | grep -qi 'compacted\|memory files'; then
    harness_fail "compaction did not run: $COMPACT_OUT"
fi

# Wait for any async work to settle.
sleep 5

# ── Phase 3: Post-compaction messages ─────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 3: Post-compaction messages ==="
for i in $(seq 1 3); do
    send_msg "Post-compact $i: what is $((RANDOM % 100)) + $((RANDOM % 100))?"
done

# ── Phase 4: Verify ──────────────────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 4: Verify ==="

# The post-compaction messages should still have cache_r > 0
# (system + tools prefix is unchanged). The value will be lower
# than pre-compaction because message-level cache was invalidated.
LAST_3="$(grep '"type":"response"' "$(forensics_path)" | tail -3)"
ALL_HAVE_READS=true
while IFS= read -r line; do
    cr="$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('cache_read_tokens', 0))" 2>/dev/null)" || cr=0
    cw="$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('cache_creation_tokens', 0))" 2>/dev/null)" || cw=0
    echo -e "${CYAN}[$TEST_NAME]${NC}   post-compact: cache_r=$cr cache_w=$cw"
    if [[ "$cr" -eq 0 ]]; then
        ALL_HAVE_READS=false
    fi
done <<< "$LAST_3"

if [[ "$ALL_HAVE_READS" == "true" ]]; then
    echo -e "${GREEN}[$TEST_NAME]${NC} post-compaction messages all have cache_r > 0"
else
    harness_fail "post-compaction messages have cache_r=0 (system prefix lost)"
fi

harness_pass
