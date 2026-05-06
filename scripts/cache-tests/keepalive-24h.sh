#!/usr/bin/env bash
#
# 24-hour cache keepalive endurance test.
#
# Starts an isolated daemon with autonomy enabled, sends an initial
# message to warm the cache, then monitors keepalive behavior for 24h.
# Every ~70 minutes sends a probe message to verify cache warmth.
# Every ~4-5 hours sends a cluster of 2-3 user messages (simulating
# real use). Logs everything to ~/Desktop/keepalive-24h-<timestamp>.log
#
# Uses Sonnet via OpenRouter with Anthropic provider pin.
# Expected cost: ~$0.05-0.10 for 24 hours.
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SHORE_BIN="$REPO_ROOT/target/debug/shore"
DAEMON_BIN="$REPO_ROOT/target/debug/shore-daemon"

# ── Config ─────────────────────────────────────────────────────────
TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
LOG_DIR="$HOME/Desktop/keepalive-test-$TIMESTAMP"
LOG_FILE="$LOG_DIR/test.log"
DAEMON_LOG="$LOG_DIR/daemon.log"
FORENSICS_LOG="$LOG_DIR/forensics.log"
DURATION_HOURS=24
PROBE_INTERVAL_SECS=4200      # 70 minutes (past the 60min TTL)
CLUSTER_INTERVAL_SECS=16200   # ~4.5 hours
CHARACTER_NAME="keepalivetest"
MODEL_ID="anthropic/claude-sonnet-4-6"
API_KEY_ENV="OPENROUTER_SHORE_TEST"

# ── Colors ─────────────────────────────────────────────────────────
RED='\033[0;31m'
GREEN='\033[0;32m'
CYAN='\033[0;36m'
YELLOW='\033[0;33m'
NC='\033[0m'

# ── Setup ──────────────────────────────────────────────────────────
mkdir -p "$LOG_DIR"

TEST_DIR="$(mktemp -d "/tmp/shore-keepalive-24h-XXXXXX")"
CONFIG_DIR="$TEST_DIR/config"
DATA_DIR="$TEST_DIR/data"
RUNTIME_DIR="$TEST_DIR/runtime"
CHAR_DIR="$CONFIG_DIR/characters/$CHARACTER_NAME"
LISTEN_ADDR="127.0.0.1:0"
DAEMON_ADDR=""
INSTANCES="$RUNTIME_DIR/instances.json"
NONCE="$(head -c 24 /dev/urandom | base64 | tr -d '/+=' | head -c 32)"

mkdir -p "$CONFIG_DIR/conf.d" "$DATA_DIR/$CHARACTER_NAME/memory" "$RUNTIME_DIR" "$CHAR_DIR"

# Copy .env from real config.
REAL_CONFIG="${SHORE_CONFIG_DIR:-${XDG_CONFIG_HOME:-$HOME/.config}/shore}"
[[ -f "$REAL_CONFIG/.env" ]] && cp "$REAL_CONFIG/.env" "$CONFIG_DIR/.env"

log() {
    local msg="[$(date '+%Y-%m-%d %H:%M:%S')] $*"
    echo -e "$msg" | tee -a "$LOG_FILE"
}

log "${CYAN}=== 24h Cache Keepalive Endurance Test ===${NC}"
log "dir: $TEST_DIR"
log "logs: $LOG_DIR"
log "nonce: $NONCE"
log "duration: ${DURATION_HOURS}h"

registered_daemon_addr() {
    local registry="$1"
    local pid="$2"
    [[ -f "$registry" ]] || return 1
    REGISTRY="$registry" DAEMON_PID="$pid" python3 - <<'PY'
import json
import os

with open(os.environ["REGISTRY"], "r", encoding="utf-8") as f:
    entries = json.load(f)
pid = int(os.environ["DAEMON_PID"])
for entry in entries:
    if entry.get("pid") == pid and entry.get("addr"):
        print(entry["addr"])
        raise SystemExit(0)
raise SystemExit(1)
PY
}

# ── Write config ───────────────────────────────────────────────────
cat > "$CONFIG_DIR/config.toml" << TOML
[defaults]
display_name = "tester"
model        = "chat.test.model"

[behavior.autonomy]
enabled = true

[behavior.autonomy.heartbeat]
enabled                          = true
fallback_heartbeat_interval    = "2h"
dormant_after_heartbeat_turns  = 50
dormant_after_idle_time          = "48h"
minimum_heartbeat_latency      = "2h"
max_tool_rounds                  = 0

[behavior.tool_use.tools]
search_history = false
read = false
write = false
edit = false
list_files = false
search = false
delete = false
exec = false

[advanced]
api_payload_logging = true

[daemon]
addr = "$LISTEN_ADDR"
TOML

cat > "$CONFIG_DIR/conf.d/models.toml" << TOML
[chat.test.model]
sdk                 = "anthropic"
model_id            = "$MODEL_ID"
api_key_env         = "$API_KEY_ENV"
base_url            = "https://openrouter.ai/api/v1"
cache_ttl           = "1h"
openrouter_provider = {order = ["Anthropic"]}
TOML

# Character with padding to exceed cache minimum.
cat > "$CHAR_DIR/character.md" << CHAREOF
You are a minimal test character for cache keepalive validation. Respond briefly.

NONCE: $NONCE

--- BEGIN PADDING ---

This padding exists to ensure the system prompt exceeds Anthropic's 2048-token
minimum for prompt caching on Sonnet. The content below is stable reference material.

Section 1: Cache Validation Principles
Prompt caching reduces redundant computation when the same token prefix appears
across multiple API calls. Cache entries have a configurable TTL of 1 hour.
Cache writes cost 25% more than base input pricing. Cache reads cost 90% less.

Section 2: Cache Testing Methodology
Key metrics: cache_read_tokens and cache_creation_tokens in the usage object.
A cache hit shows cache_read_tokens > 0. A cache miss shows cache_creation_tokens > 0.

Section 3: Keepalive Economics
Each keepalive ping costs 0.1N tokens (cached read). A cache miss costs 1.9N tokens.
Break-even at ~19 pings (19 hours). The daemon pings every 55 minutes with 5 minutes
of headroom before the 60-minute TTL expires.

Section 4: Operational Parameters
Cache TTL: 1 hour. Keepalive interval: 55 minutes. Minimum cacheable prefix: 2048 tokens.
The cache_control annotation uses type ephemeral with optional ttl parameter.

Section 5: Token Economics
Common English words are single tokens. On average one token equals approximately
3.5-4 characters. Cache write premium is 25% over base. Cache read discount is 90%.

Section 6: API Response Structure
The usage object contains input_tokens, output_tokens, cache_creation_input_tokens,
and cache_read_input_tokens. Streaming uses SSE with event types: message_start,
content_block_start, content_block_delta, content_block_stop, message_delta, message_stop.

Section 7: Additional Padding
The model field specifies which Claude model to use. The max_tokens field sets the upper
bound on output tokens. The messages field contains conversation history. Content blocks
can be text, image, tool_use, or tool_result. The system parameter accepts a string or
array of content blocks. Temperature and top_p control output randomness. HTTP headers
include anthropic-version, x-api-key, and content-type.

--- END PADDING ---

Remember: respond briefly. Do not reference the padding material.
CHAREOF

# Memory index to ensure heartbeat has context for scheduling.
mkdir -p "$CONFIG_DIR/characters/$CHARACTER_NAME/workspace/memory"
cat > "$CONFIG_DIR/characters/$CHARACTER_NAME/workspace/memory/MEMORY.md" << 'RECAP'
# Memory Index

This is a test character used for cache keepalive validation. The user sends
periodic test messages to verify cache warmth. Respond briefly to each message.
RECAP

# ── Start daemon ───────────────────────────────────────────────────
log "Building..."
cargo build --bin shore --bin shore-daemon \
    --manifest-path "$REPO_ROOT/Cargo.toml" 2>&1 | tail -3

log "Starting daemon..."
SHORE_CONFIG_DIR="$CONFIG_DIR" \
SHORE_DATA_DIR="$DATA_DIR" \
SHORE_RUNTIME_DIR="$RUNTIME_DIR" \
RUST_LOG=info,shore_daemon::autonomy=debug,shore_llm::providers::anthropic=debug \
    "$DAEMON_BIN" > "$DAEMON_LOG" 2>&1 &
DAEMON_PID=$!

# Wait for the daemon to publish its resolved TCP address.
tries=0
while [[ $tries -lt 30 ]]; do
    DAEMON_ADDR="$(registered_daemon_addr "$INSTANCES" "$DAEMON_PID" 2>/dev/null || true)"
    [[ -n "$DAEMON_ADDR" ]] && break
    sleep 0.5
    tries=$((tries + 1))
done

if [[ -z "$DAEMON_ADDR" ]]; then
    log "${RED}Daemon failed to start${NC}"
    tail -20 "$DAEMON_LOG" >> "$LOG_FILE"
    exit 1
fi
log "Daemon running (PID $DAEMON_PID, addr $DAEMON_ADDR)"

# ── Cleanup trap ───────────────────────────────────────────────────
cleanup() {
    local exit_code=$?
    log "Stopping daemon..."
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
    # Copy forensics to log dir.
    cp "$DATA_DIR/cache_forensics.jsonl" "$FORENSICS_LOG" 2>/dev/null || true
    # Extract keepalive events from daemon log.
    grep -i 'keepalive\|dormant.*ping\|cache.*refresh' "$DAEMON_LOG" > "$LOG_DIR/keepalive-events.log" 2>/dev/null || true
    log "Test complete. Logs at: $LOG_DIR"
    if [[ $exit_code -ne 0 ]]; then
        log "${YELLOW}Preserving test dir: $TEST_DIR${NC}"
    else
        rm -rf "$TEST_DIR"
    fi
}
trap cleanup EXIT

# ── Helper: send message and log cache metrics ────────────────────
send_and_log() {
    local msg="$1"
    local label="$2"
    log "${CYAN}[$label]${NC} sending: $msg"

    local output
    output=$(SHORE_CONFIG_DIR="$CONFIG_DIR" \
             SHORE_DATA_DIR="$DATA_DIR" \
             "$SHORE_BIN" --addr "$DAEMON_ADDR" \
                 --character "$CHARACTER_NAME" \
                 send "$msg" 2>>"$DAEMON_LOG")
    echo "$output" | head -5 >> "$LOG_FILE"

    # Parse cache metrics from forensics.
    local forensics="$DATA_DIR/cache_forensics.jsonl"
    if [[ -f "$forensics" ]]; then
        local last_resp
        last_resp="$(grep '"type":"response"' "$forensics" | tail -1)"
        if [[ -n "$last_resp" ]]; then
            local cache_r cache_w input
            cache_r="$(echo "$last_resp" | python3 -c "import sys,json; print(json.load(sys.stdin).get('cache_read_tokens',0))" 2>/dev/null)" || cache_r="?"
            cache_w="$(echo "$last_resp" | python3 -c "import sys,json; print(json.load(sys.stdin).get('cache_creation_tokens',0))" 2>/dev/null)" || cache_w="?"
            input="$(echo "$last_resp" | python3 -c "import sys,json; print(json.load(sys.stdin).get('input_tokens',0))" 2>/dev/null)" || input="?"
            log "  cache_r=$cache_r cache_w=$cache_w input=$input"

            if [[ "$cache_w" != "?" && "$cache_w" -gt 1000 && "$label" != "warmup" ]]; then
                log "${RED}  *** CACHE COLD! Full rewrite detected ***${NC}"
            elif [[ "$cache_r" != "?" && "$cache_r" -gt 0 ]]; then
                log "${GREEN}  cache warm ✓${NC}"
            fi
        fi
    fi
}

# ── Initial warm-up ───────────────────────────────────────────────
log ""
log "=== Phase 1: Warm-up ==="
send_and_log "Hello! This is the cache keepalive endurance test. Nonce: $NONCE" "warmup"
sleep 2
send_and_log "Confirm cache warm. What is 7 plus 3?" "warmup-verify"

# ── Main loop ─────────────────────────────────────────────────────
log ""
log "=== Phase 2: Endurance (${DURATION_HOURS}h) ==="
log "  Probes every ${PROBE_INTERVAL_SECS}s (~70min)"
log "  Message clusters every ${CLUSTER_INTERVAL_SECS}s (~4.5h)"

START_TIME=$(date +%s)
END_TIME=$((START_TIME + DURATION_HOURS * 3600))
PROBE_COUNT=0
CLUSTER_COUNT=0
LAST_CLUSTER=$START_TIME

while [[ $(date +%s) -lt $END_TIME ]]; do
    # Check daemon is still alive.
    if ! kill -0 "$DAEMON_PID" 2>/dev/null; then
        log "${RED}Daemon died! Check $DAEMON_LOG${NC}"
        exit 1
    fi

    # Wait for next probe.
    sleep "$PROBE_INTERVAL_SECS"
    PROBE_COUNT=$((PROBE_COUNT + 1))

    NOW=$(date +%s)
    ELAPSED=$(( (NOW - START_TIME) / 60 ))

    # Send probe.
    send_and_log "Keepalive probe #$PROBE_COUNT at +${ELAPSED}min. What is $((RANDOM % 100)) plus $((RANDOM % 100))?" "probe-$PROBE_COUNT"

    # Check if it's time for a message cluster.
    if [[ $((NOW - LAST_CLUSTER)) -ge $CLUSTER_INTERVAL_SECS ]]; then
        CLUSTER_COUNT=$((CLUSTER_COUNT + 1))
        log "${CYAN}--- Message cluster #$CLUSTER_COUNT ---${NC}"
        sleep 3
        send_and_log "Cluster $CLUSTER_COUNT msg 1: Tell me something interesting." "cluster-$CLUSTER_COUNT-1"
        sleep 5
        send_and_log "Cluster $CLUSTER_COUNT msg 2: What is $((RANDOM % 1000)) times $((RANDOM % 100))?" "cluster-$CLUSTER_COUNT-2"
        sleep 3
        send_and_log "Cluster $CLUSTER_COUNT msg 3: Thanks, continue monitoring." "cluster-$CLUSTER_COUNT-3"
        LAST_CLUSTER=$NOW
    fi

    # Log keepalive events since last probe.
    local_keepalive_count=$(grep -c 'keepalive ping\|dormant.*ping\|cache.*refresh' "$DAEMON_LOG" 2>/dev/null || echo 0)
    log "  (total keepalive events in daemon log: $local_keepalive_count)"
done

# ── Summary ────────────────────────────────────────────────────────
log ""
log "=== Final Summary ==="
log "Duration: ${DURATION_HOURS}h"
log "Probes sent: $PROBE_COUNT"
log "Message clusters: $CLUSTER_COUNT"

# Count keepalive pings from daemon log.
PING_COUNT=$(grep -c 'Cache keepalive ping\|cache.*refresh' "$DAEMON_LOG" 2>/dev/null || echo 0)
log "Keepalive pings fired: $PING_COUNT"

# Count cache misses from forensics.
if [[ -f "$DATA_DIR/cache_forensics.jsonl" ]]; then
    TOTAL_CALLS=$(grep -c '"type":"response"' "$DATA_DIR/cache_forensics.jsonl" 2>/dev/null || echo 0)
    COLD_STARTS=$(grep '"type":"response"' "$DATA_DIR/cache_forensics.jsonl" | \
        python3 -c "
import sys, json
cold = 0
for i, line in enumerate(sys.stdin):
    if i == 0: continue  # skip cold start
    d = json.loads(line)
    if d.get('cache_creation_tokens', 0) > 1000 and d.get('cache_read_tokens', 0) == 0:
        cold += 1
print(cold)
" 2>/dev/null || echo "?")
    log "Total API calls: $TOTAL_CALLS"
    log "Cold-start misses (excluding first): $COLD_STARTS"
fi

if [[ "${COLD_STARTS:-0}" == "0" ]]; then
    log "${GREEN}PASS — cache stayed warm for the entire test${NC}"
else
    log "${RED}FAIL — $COLD_STARTS cache misses detected${NC}"
fi

log "Full logs: $LOG_DIR/"
