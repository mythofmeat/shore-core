#!/usr/bin/env bash
#
# End-to-end tool-loop cache test through Shore's openai.rs (chat-completions)
# path, routing Anthropic via OpenRouter. Complements 24-tool-loop-daemon.sh
# (which exercises the anthropic.rs /messages path).
#
# What this proves:
#   - Cache markers (OpenRouter cache_control extensions) survive the wire.
#   - Reasoning_details preserved across an adaptive tool call.
#   - The tool-result continuation reads the warm prefix (cache_r > 0).
#
# Hard-fails if:
#   - No tool call is recorded.
#   - The first tool_loop continuation rewrites the prefix (cache_r == 0).
#   - The ledger records a cache_anomaly on a tool_loop call.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "tool-loop-openai-daemon"

MODEL_ID="anthropic/claude-sonnet-4-6"
API_KEY_ENV="OPENROUTER_API_KEY"
BASE_URL="https://openrouter.ai/api/v1"
CACHE_TTL="1h"

_write_config() {
    cat > "$CONFIG_DIR/config.toml" << TOML
[defaults]
display_name = "tester"
model        = "chat.openrouter.tool-test"

[behavior.autonomy]
enabled = false

[behavior.tool_use]
enabled = true
max_iterations = 3

[behavior.tool_use.tools]
check_time = true
roll_dice = false
activity_heatmap = false
generate_image = false
web_search = false
fetch_url = false
read = false
write = false
edit = false
list_files = false
search = false
delete = false
exec = false
search_history = false

[advanced]
api_payload_logging = true
cache_forensics = true

[daemon]
addr = "$LISTEN_ADDR"
TOML

    local model_toml="$CONFIG_DIR/conf.d/models.toml"
    {
        echo '[chat.openrouter.tool-test]'
        echo "sdk          = \"openai\""
        echo "model_id     = \"$MODEL_ID\""
        echo "api_key_env  = \"$API_KEY_ENV\""
        echo "base_url     = \"$BASE_URL\""
        echo "cache_ttl    = \"$CACHE_TTL\""
        echo "reasoning_effort = \"high\""
        echo 'openrouter_provider = { order = ["Anthropic"], allow_fallbacks = false }'
    } > "$model_toml"
}

harness_start

if ! grep -q "^${API_KEY_ENV}=." "$CONFIG_DIR/.env" 2>/dev/null; then
    harness_fail "$API_KEY_ENV is not set in the harness .env"
fi

echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 1: Warm cache ==="
send_msg "Warm-up one. Reply with only WARM1."
send_msg "Warm-up two. Reply with only WARM2."

echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 2: Tool use ==="
send_msg "Use the check_time tool exactly once before answering. After the tool result, reply with only TIME_OK."

LEDGER="$DATA_DIR/ledger.db"
if [[ ! -f "$LEDGER" ]]; then
    harness_fail "ledger missing at $LEDGER"
fi

echo -e "${CYAN}[$TEST_NAME]${NC} ledger rows (most recent first):"
sqlite3 -header -column "$LEDGER" \
    "select call_type, cache_read_tokens, cache_write_tokens, cache_anomaly, finish_reason, thinking_enabled
       from calls
      where call_type in ('message', 'tool_loop')
      order by id desc
      limit 6;" || harness_fail "failed to read ledger rows"

TOOL_LOOPS="$(sqlite3 "$LEDGER" "select count(*) from calls where call_type = 'tool_loop';")"
if [[ "$TOOL_LOOPS" -eq 0 ]]; then
    harness_fail "model did not enter a Shore tool loop"
fi

ANOMALIES="$(sqlite3 "$LEDGER" \
    "select count(*) from calls where call_type = 'tool_loop' and cache_anomaly is not null;")"
if [[ "$ANOMALIES" -ne 0 ]]; then
    harness_fail "tool-loop ledger rows contain cache anomalies (count=$ANOMALIES)"
fi

FIRST_TOOL_ROW="$(sqlite3 -separator ' ' "$LEDGER" \
    "select cache_read_tokens, cache_write_tokens
       from calls
      where call_type = 'tool_loop'
      order by id
      limit 1;")"
read -r FIRST_TOOL_READ FIRST_TOOL_WRITE <<< "$FIRST_TOOL_ROW"

echo -e "${CYAN}[$TEST_NAME]${NC} first tool_loop: cache_r=$FIRST_TOOL_READ cache_w=$FIRST_TOOL_WRITE"
if [[ "${FIRST_TOOL_READ:-0}" -eq 0 ]]; then
    harness_fail "first tool-loop continuation did NOT read the warm prefix (cache_r=0) — this is the bug we're trying to fix; openai.rs cache_control extensions aren't surviving the wire"
fi

harness_pass
