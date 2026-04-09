#!/usr/bin/env bash
#
# Shared test harness for cache tests.
#
# Each test script sources this file, which provides:
#   - A fresh temp dir with config, data, character, and socket
#   - Functions: start_daemon, stop_daemon, send_msg, check_forensics
#   - Automatic cleanup on exit
#
# Usage in a test script:
#   source "$(dirname "$0")/harness.sh"
#   harness_init "test-name"
#   # ... configure cache_depth_turns, cache_pinned_position, etc.
#   harness_start
#   send_msg "hello"
#   send_msg "world"
#   check_no_unexpected_writes
#   harness_pass
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SHORE_BIN="$REPO_ROOT/target/debug/shore"
DAEMON_BIN="$REPO_ROOT/target/debug/shore-daemon"

# ── Colors ──────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# ── State ───────────────────────────────────────────────────────────
TEST_NAME=""
TEST_DIR=""
DAEMON_PID=""
SOCKET_PATH=""
CONFIG_DIR=""
DATA_DIR=""
CHAR_DIR=""
LOG_FILE=""
NONCE=""

# Test config (set by individual tests before harness_start)
CACHE_DEPTH_TURNS=""
CACHE_PINNED_POSITION=""
CACHE_TTL="1h"
REASONING_EFFORT=""
MODEL_ID="anthropic/claude-sonnet-4-6"
API_KEY_ENV="OPENROUTER_SHORE_TEST"
BASE_URL="https://openrouter.ai/api/v1"
CHARACTER_NAME="cachetest"
OPENROUTER_PROVIDER=""

# ── Lifecycle ───────────────────────────────────────────────────────

harness_init() {
    TEST_NAME="$1"
    NONCE="$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | head -c 32)"

    TEST_DIR="$(mktemp -d "/tmp/shore-cache-test-${TEST_NAME}-XXXXXX")"
    CONFIG_DIR="$TEST_DIR/config"
    DATA_DIR="$TEST_DIR/data"
    CHAR_DIR="$CONFIG_DIR/characters/$CHARACTER_NAME"
    SOCKET_PATH="$TEST_DIR/shore.sock"
    LOG_FILE="$TEST_DIR/daemon.log"

    mkdir -p "$CONFIG_DIR/conf.d" "$DATA_DIR" "$CHAR_DIR"

    # Copy .env from the real config dir so API keys are available.
    local real_config_dir="${SHORE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/shore}"
    if [[ -f "$real_config_dir/.env" ]]; then
        cp "$real_config_dir/.env" "$CONFIG_DIR/.env"
    fi

    echo -e "${CYAN}[$TEST_NAME]${NC} init: $TEST_DIR"
    echo -e "${CYAN}[$TEST_NAME]${NC} nonce: $NONCE"

    # Trap cleanup on any exit.
    trap _harness_cleanup EXIT
}

harness_start() {
    _write_config
    _write_character
    _build
    _start_daemon
}

harness_pass() {
    echo -e "${GREEN}[$TEST_NAME] PASS${NC}"
    # Print forensics summary.
    _print_forensics_summary
    exit 0
}

harness_fail() {
    local reason="${1:-unknown}"
    echo -e "${RED}[$TEST_NAME] FAIL: $reason${NC}"
    _print_forensics_summary
    exit 1
}

# ── Messaging ───────────────────────────────────────────────────────

# Track message count and first write size for inline checks.
_MSG_INDEX=0
_FIRST_WRITE=0
# Write threshold: any cache write above this after the first message
# is a hard failure. Default: 0 (set after first message).
_WRITE_THRESHOLD=0

send_msg() {
    local msg="$1"
    echo -e "${CYAN}[$TEST_NAME]${NC} send: $msg"
    SHORE_CONFIG_DIR="$CONFIG_DIR" \
    SHORE_DATA_DIR="$DATA_DIR" \
        "$SHORE_BIN" --socket "$SOCKET_PATH" \
            --character "$CHARACTER_NAME" \
            send "$msg" 2>>"$LOG_FILE"
    echo ""

    # Inline check: read the latest response from forensics.
    _check_latest_response
    _MSG_INDEX=$((_MSG_INDEX + 1))
}

_check_latest_response() {
    local path
    path="$(forensics_path)"
    [[ -f "$path" ]] || return

    local last_resp
    last_resp="$(grep '"type":"response"' "$path" | tail -1)"
    [[ -n "$last_resp" ]] || return

    local write read
    write="$(echo "$last_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('cache_creation_tokens', 0))" 2>/dev/null)" || return
    read="$(echo "$last_resp" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('cache_read_tokens', 0))" 2>/dev/null)" || return

    if [[ $_MSG_INDEX -eq 0 ]]; then
        # First message: must be a write (cold start).
        _FIRST_WRITE="$write"
        # Threshold: half the first write. Any write bigger than this
        # after the first message is a full prefix rewrite = failure.
        _WRITE_THRESHOLD=$((_FIRST_WRITE / 2))
        echo -e "${CYAN}[$TEST_NAME]${NC}   turn 0: cache_w=$write (cold start, threshold=$_WRITE_THRESHOLD)"
    else
        echo -e "${CYAN}[$TEST_NAME]${NC}   turn $_MSG_INDEX: cache_r=$read cache_w=$write"
        if [[ "$write" -gt "$_WRITE_THRESHOLD" ]]; then
            harness_fail "turn $_MSG_INDEX: cache write $write exceeds threshold $_WRITE_THRESHOLD (first_write=$_FIRST_WRITE, read=$read)"
        fi
    fi
}

# ── Forensics ───────────────────────────────────────────────────────

# Returns the forensics JSONL path.
forensics_path() {
    echo "$DATA_DIR/cache_forensics.jsonl"
}

# ── Internal ────────────────────────────────────────────────────────

_write_config() {
    # Main config.
    cat > "$CONFIG_DIR/config.toml" << TOML
[defaults]
display_name = "tester"
model        = "chat.test.model"

[behavior.autonomy]
enabled = false

[behavior.tool_use.tools]
memory = true

[advanced]
api_payload_logging = true

[daemon]
socket_path = "$SOCKET_PATH"
TOML

    # Model config.
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

_write_character() {
    # Minimal character that exceeds the 1024-token Anthropic cache minimum.
    cat > "$CHAR_DIR/character.md" << 'CHAREOF'
You are a minimal test character for cache validation. Respond briefly.

NONCE: __NONCE__

--- BEGIN PADDING ---

This padding exists to ensure the system prompt exceeds Anthropic's 1024-token
minimum for prompt caching. The content below is stable reference material.

Section 1: Cache Validation Principles

Prompt caching reduces redundant computation when the same token prefix appears
across multiple API calls. The API compares incoming tokens from the beginning
and serves matching prefixes from cache. Cache entries have a configurable TTL.
Cache writes cost 25% more than base input pricing. Cache reads cost 90% less.
For a 1-hour TTL, up to 19 keepalive pings are economically justified.

Section 2: Cache Testing Methodology

Key metrics: cache_read_tokens and cache_creation_tokens in the usage object.
A cache hit shows cache_read_tokens > 0 and cache_creation_tokens = 0.
A cache miss shows cache_creation_tokens > 0. The prefix hash helps identify
whether content changed between calls. Breakpoint position should remain
consistent across calls for stable caching.

Section 3: Failure Modes

Thinking mode changes invalidate the prefix. Content format normalization
between string and array formats causes cache invalidation. Cache marker
movement does not invalidate the prefix (markers are directives not content).
Routing instability through proxies can cause server-side misses. TTL
expiration clears the entry.

Section 4: Operational Parameters

Cache TTL: 1 hour. Keepalive interval: 59 minutes. Minimum cacheable prefix:
1024 tokens. The cache_control annotation uses type ephemeral with optional
ttl parameter. Multiple breakpoints can exist per request up to a maximum of 4.

Section 5: Token Economics

The Anthropic Messages API uses byte-pair encoding tokenization. Common English
words are single tokens. Rare words and technical terms may need multiple tokens.
On average one token equals approximately 3.5-4 characters of English text.
Cache write premium is 25% over base. Cache read discount is 90% off base.
Break-even depends on reuse count within the TTL window.

Section 6: API Response Structure

The usage object contains input_tokens, output_tokens, cache_creation_input_tokens,
and cache_read_input_tokens. The streaming interface uses SSE with event types:
message_start, content_block_start, content_block_delta, content_block_stop,
message_delta, and message_stop.

Section 7: Additional Stable Padding

The model field specifies which Claude model to use. The max_tokens field sets
the upper bound on output tokens. The messages field contains conversation history.
Content blocks can be text, image, tool_use, or tool_result. The system parameter
accepts a string or array of content blocks. Temperature and top_p control output
randomness. HTTP headers include anthropic-version, x-api-key, and content-type.
Error types include invalid_request_error, authentication_error, rate_limit_error,
and overloaded_error. Rate limits include retry-after headers.

--- END PADDING ---

Remember: respond briefly. Do not reference the padding material.
CHAREOF

    # Inject the unique nonce.
    sed -i "s/__NONCE__/$NONCE/" "$CHAR_DIR/character.md"
}

_build() {
    echo -e "${CYAN}[$TEST_NAME]${NC} building..."
    cargo build --bin shore --bin shore-daemon \
        --manifest-path "$REPO_ROOT/Cargo.toml" 2>&1 | tail -3
}

_start_daemon() {
    echo -e "${CYAN}[$TEST_NAME]${NC} starting daemon..."
    # Cache breakpoint config via env vars (overrides hardcoded defaults).
    local cache_env=""
    [[ -n "$CACHE_DEPTH_TURNS" ]] && cache_env="SHORE_CACHE_DEPTH_TURNS=$(echo "$CACHE_DEPTH_TURNS" | tr -d '[] ')"
    [[ -n "$CACHE_PINNED_POSITION" ]] && cache_env="$cache_env SHORE_CACHE_PINNED_POSITION=$(echo "$CACHE_PINNED_POSITION" | tr -d '[] ')"

    env $cache_env \
    SHORE_CONFIG_DIR="$CONFIG_DIR" \
    SHORE_DATA_DIR="$DATA_DIR" \
    RUST_LOG=info,shore_daemon::autonomy=debug,shore_llm_client::providers::anthropic=debug \
        "$DAEMON_BIN" > "$LOG_FILE" 2>&1 &
    DAEMON_PID=$!

    # Wait for socket to appear.
    local tries=0
    while [[ ! -S "$SOCKET_PATH" && $tries -lt 20 ]]; do
        sleep 0.25
        tries=$((tries + 1))
    done

    if [[ ! -S "$SOCKET_PATH" ]]; then
        echo -e "${RED}[$TEST_NAME]${NC} daemon failed to start. Log:"
        tail -20 "$LOG_FILE"
        exit 1
    fi
    echo -e "${CYAN}[$TEST_NAME]${NC} daemon running (PID $DAEMON_PID)"
}

stop_daemon() {
    if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
        DAEMON_PID=""
    fi
}

_harness_cleanup() {
    local exit_code=$?
    stop_daemon
    if [[ $exit_code -ne 0 && -f "$LOG_FILE" ]]; then
        echo -e "${YELLOW}[$TEST_NAME]${NC} daemon log (last 30 lines):"
        tail -30 "$LOG_FILE" 2>/dev/null || true
    fi
    if [[ $exit_code -eq 0 ]]; then
        echo -e "${CYAN}[$TEST_NAME]${NC} cleaning up $TEST_DIR"
        rm -rf "$TEST_DIR"
    else
        echo -e "${YELLOW}[$TEST_NAME]${NC} preserving $TEST_DIR for debugging"
    fi
}

_print_forensics_summary() {
    local path
    path="$(forensics_path)"
    if [[ ! -f "$path" ]]; then
        echo -e "${YELLOW}[$TEST_NAME]${NC} no forensics log found"
        return
    fi
    echo -e "${CYAN}[$TEST_NAME]${NC} forensics summary:"
    grep '"type":"response"' "$path" | \
        python3 -c "
import sys, json
for i, line in enumerate(sys.stdin):
    d = json.loads(line)
    r = d.get('cache_read_tokens', 0)
    w = d.get('cache_creation_tokens', 0)
    inp = d.get('input_tokens', 0)
    tag = '  WRITE' if w > 0 else ''
    print(f'  [{i}] input={inp} cache_r={r} cache_w={w}{tag}')
" 2>/dev/null || echo "  (failed to parse)"
}
