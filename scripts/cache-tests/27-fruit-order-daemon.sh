#!/usr/bin/env bash
#
# End-to-end fruit-order assertion: drive a Shore daemon configured to
# route Anthropic-via-OpenRouter through the /chat/completions path,
# inject mid-history system messages, and verify the model lists them
# in their actual conversation position (not lifted to the top).
#
# Failure case (the bug we're guarding against): if the openai.rs path
# stops wrapping inline `role: "system"` messages, OpenRouter would
# re-order them and the model would say something like
# `apple banana peach grape orange` (grape after peach) — or worse,
# `grape orange apple banana peach` if collapsed to the top system block.
#
# This is a real live test: it builds Shore, starts the daemon, talks to
# OpenRouter, and asserts on the assistant's reply.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "fruit-order"

# Pin the model: Anthropic via OpenRouter, openai SDK so we exercise the
# /chat/completions wire path we just added cache+nested-reasoning to.
MODEL_ID="anthropic/claude-sonnet-4-6"
API_KEY_ENV="OPENROUTER_API_KEY"
BASE_URL="https://openrouter.ai/api/v1"
CACHE_TTL="1h"

_write_config() {
    cat > "$CONFIG_DIR/config.toml" << TOML
[defaults]
display_name = "tester"
model        = "chat.openrouter.fruit-test"

[behavior.autonomy]
enabled = false

[behavior.tool_use]
enabled = false

[advanced]
api_payload_logging = true
cache_forensics = true

[daemon]
addr = "$LISTEN_ADDR"
TOML

    # Provider key comes from the second TOML segment (chat.{provider_key}.{name})
    # so use `openrouter` here to get the OpenRouter ProviderContext defaults.
    local model_toml="$CONFIG_DIR/conf.d/models.toml"
    {
        echo '[chat.openrouter.fruit-test]'
        echo "sdk          = \"openai\""
        echo "model_id     = \"$MODEL_ID\""
        echo "api_key_env  = \"$API_KEY_ENV\""
        echo "base_url     = \"$BASE_URL\""
        echo "cache_ttl    = \"$CACHE_TTL\""
        echo "reasoning_effort = \"high\""
        echo 'openrouter_provider = { order = ["Anthropic"], allow_fallbacks = false }'
    } > "$model_toml"
}

# Capture stdout so we can assert on the assistant's reply.
send_msg_for_reply() {
    local msg="$1"
    SHORE_CONFIG_DIR="$CONFIG_DIR" \
    SHORE_DATA_DIR="$DATA_DIR" \
        "$SHORE_BIN" --addr "$DAEMON_ADDR" \
            --character "$CHARACTER_NAME" \
            send "$msg" 2>>"$LOG_FILE"
}

send_sys_msg() {
    local msg="$1"
    echo -e "${CYAN}[$TEST_NAME]${NC} inject_system: $msg"
    SHORE_CONFIG_DIR="$CONFIG_DIR" \
    SHORE_DATA_DIR="$DATA_DIR" \
        "$SHORE_BIN" --addr "$DAEMON_ADDR" \
            --character "$CHARACTER_NAME" \
            send --system "$msg" 2>>"$LOG_FILE" > /dev/null
}

harness_start

# ── Verify the OpenRouter API key is actually available to the daemon ──
if ! grep -q "^${API_KEY_ENV}=." "$CONFIG_DIR/.env" 2>/dev/null; then
    harness_fail "$API_KEY_ENV is not set in the harness .env (copied from \$XDG_CONFIG_HOME/shore/.env at init)"
fi

# ── Phase 1: build the conversation ──────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === building fruit conversation ==="
send_msg "I'm going to list 5 fruits."
send_msg "apple"
send_msg "banana"
send_sys_msg "grape"
send_msg "peach"
send_sys_msg "orange"

# ── Phase 2: ask for the order ───────────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === asking for order ==="
PROMPT="List every fruit you have seen so far in this conversation, in the EXACT order they appeared. Output ONE FRUIT PER LINE with no commentary, no numbering, and no other text."
REPLY="$(send_msg_for_reply "$PROMPT")"

echo -e "${CYAN}[$TEST_NAME]${NC} assistant reply:"
echo "$REPLY" | sed 's/^/  /'

# ── Phase 3: extract order ───────────────────────────────────────────
# Find the first occurrence of each fruit (case-insensitive) and order
# them by position.
EXPECTED=(apple banana grape peach orange)
declare -A POS
LOWER="$(echo "$REPLY" | tr '[:upper:]' '[:lower:]')"

for fruit in "${EXPECTED[@]}"; do
    # awk index() returns 1-based offset of first match (0 if absent).
    p="$(awk -v needle="$fruit" 'BEGIN{exit} {print index($0, needle); exit}' <<<"$LOWER" || true)"
    # Fall back to a portable per-line scan if the awk one-liner gave 0.
    if [[ -z "$p" || "$p" == "0" ]]; then
        p="$(awk -v needle="$fruit" '{i=index($0, needle); if (i>0) {print NR*1000 + i; exit}}' <<<"$LOWER")"
    fi
    if [[ -z "$p" || "$p" == "0" ]]; then
        harness_fail "fruit '$fruit' missing from reply"
    fi
    POS[$fruit]="$p"
done

# Sort fruits by position to get the order the model emitted.
ORDER_OBSERVED=()
while IFS= read -r line; do
    ORDER_OBSERVED+=("${line#* }")
done < <(
    for fruit in "${EXPECTED[@]}"; do
        printf '%010d %s\n' "${POS[$fruit]}" "$fruit"
    done | sort -n
)

echo -e "${CYAN}[$TEST_NAME]${NC} expected order: ${EXPECTED[*]}"
echo -e "${CYAN}[$TEST_NAME]${NC} observed order: ${ORDER_OBSERVED[*]}"

if [[ "${ORDER_OBSERVED[*]}" != "${EXPECTED[*]}" ]]; then
    harness_fail "fruit order WRONG — got '${ORDER_OBSERVED[*]}', expected '${EXPECTED[*]}'. This means inline system messages were re-ordered or collapsed."
fi

# ── Phase 4: cache sanity ────────────────────────────────────────────
# The integrated test isn't primarily a cache test, but the model entry
# enables caching, so we should at least see one cache_read > 0 after
# the early cold turn.
LEDGER="$DATA_DIR/ledger.db"
if [[ -f "$LEDGER" ]]; then
    READS="$(sqlite3 "$LEDGER" \
        "select count(*) from calls where call_type = 'message' and cache_read_tokens > 0;" || echo 0)"
    echo -e "${CYAN}[$TEST_NAME]${NC} message calls with cache_read>0: $READS"
fi

harness_pass
