#!/usr/bin/env bash
#
# Test: the first tool-loop continuation after a warm chat message does not
# rewrite the warm Anthropic message prefix when Shore drives the provider.
#
# This complements 19-tool-loop.py, which constructs direct Anthropic payloads.
# Here a temporary Shore daemon renders the prompt, tool surface, and cache
# breakpoints, then the ledger checks the real tool_loop calls. Set
# TOOL_LOOP_THINKING_MODE=adaptive to exercise reasoning_effort alone.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "tool-loop-daemon"

BUDGET_TOKENS=4096
TOOL_LOOP_THINKING_MODE="${TOOL_LOOP_THINKING_MODE:-budget}"
TOOL_LOOP_EFFORT="${TOOL_LOOP_EFFORT:-high}"
if [[ ! -v OPENROUTER_PROVIDER ]]; then
    OPENROUTER_PROVIDER='{ order = ["Anthropic"], allow_fallbacks = false }'
fi

_write_config() {
    cat > "$CONFIG_DIR/config.toml" << TOML
[defaults]
display_name = "tester"
model        = "chat.test.model"

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
        echo '[chat.test.model]'
        echo "sdk          = \"anthropic\""
        echo "model_id     = \"$MODEL_ID\""
        echo "api_key_env  = \"$API_KEY_ENV\""
        echo "base_url     = \"$BASE_URL\""
        echo "cache_ttl    = \"$CACHE_TTL\""
        if [[ "$TOOL_LOOP_THINKING_MODE" == "adaptive" ]]; then
            echo "reasoning_effort = \"$TOOL_LOOP_EFFORT\""
        else
            echo "budget_tokens = $BUDGET_TOKENS"
        fi
        [[ -n "$OPENROUTER_PROVIDER" ]] && echo "openrouter_provider = $OPENROUTER_PROVIDER" || true
    } > "$model_toml"
}

harness_start

echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 1: Warm cache ==="
send_msg "Warm-up one. Reply with only WARM1."
send_msg "Warm-up two. Reply with only WARM2."

echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 2: Tool use ==="
send_msg "Use the check_time tool exactly once before answering. After the tool result, reply with only TIME_OK."

LEDGER="$DATA_DIR/ledger.db"
if [[ ! -f "$LEDGER" ]]; then
    harness_fail "ledger missing at $LEDGER"
fi

echo -e "${CYAN}[$TEST_NAME]${NC} tool-loop ledger rows:"
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
    harness_fail "tool-loop ledger rows contain cache anomalies"
fi

FIRST_TOOL_ROW="$(sqlite3 -separator ' ' "$LEDGER" \
    "select cache_read_tokens, cache_write_tokens, thinking_enabled
       from calls
      where call_type = 'tool_loop'
      order by id
      limit 1;")"
read -r FIRST_TOOL_READ FIRST_TOOL_WRITE FIRST_TOOL_THINKING <<< "$FIRST_TOOL_ROW"

if [[ "${FIRST_TOOL_THINKING:-0}" -ne 1 ]]; then
    harness_fail "tool-loop call did not record thinking_enabled=1"
fi

if [[ "${FIRST_TOOL_READ:-0}" -eq 0 ]]; then
    harness_fail "first tool-loop continuation did not read the warm prefix"
fi

harness_pass
