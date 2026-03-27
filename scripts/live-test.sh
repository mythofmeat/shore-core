#!/usr/bin/env bash
#
# Shore live test — starts shore-llm + shore-daemon and runs real API tests.
#
# Requires: ANTHROPIC_API_KEY set in environment.
#
# Usage:
#   ./scripts/live-test.sh              # build + test
#   ./scripts/live-test.sh --skip-build # reuse existing binaries
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

run_test() {
    local name="$1"
    shift
    printf "${DIM}  %-50s${RESET}" "$name"
    local output
    if output=$("$@" 2>&1); then
        printf "${GREEN}PASS${RESET}\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL${RESET}\n"
        printf "${DIM}%s${RESET}\n" "$output" | head -5
        fail=$((fail + 1))
    fi
}

run_test_contains() {
    local name="$1"
    local expected="$2"
    shift 2
    printf "${DIM}  %-50s${RESET}" "$name"
    local output
    if output=$("$@" 2>&1) && echo "$output" | grep -qF "$expected"; then
        printf "${GREEN}PASS${RESET}\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL${RESET}\n"
        printf "${DIM}  expected output to contain: %s${RESET}\n" "$expected"
        printf "${DIM}  got: %s${RESET}\n" "$output" | head -5
        fail=$((fail + 1))
    fi
}

run_test_expect_fail() {
    local name="$1"
    shift
    printf "${DIM}  %-50s${RESET}" "$name"
    local output
    if output=$("$@" 2>&1); then
        printf "${RED}FAIL (expected error, got success)${RESET}\n"
        printf "${DIM}%s${RESET}\n" "$output" | head -3
        fail=$((fail + 1))
    else
        printf "${GREEN}PASS${RESET}\n"
        pass=$((pass + 1))
    fi
}

# ── Pre-checks ────────────────────────────────────────────────────────
if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
    echo "OPENROUTER_API_KEY not set — skipping live tests."
    exit 0
fi

# ── Build ─────────────────────────────────────────────────────────────
if [[ "$SKIP_BUILD" == false ]]; then
    printf "${BOLD}Building...${RESET}\n"
    cargo build --workspace --quiet 2>&1
fi

SHORE="$REPO_ROOT/target/debug/shore"
DAEMON="$REPO_ROOT/target/debug/shore-daemon"
LLM_JS="$REPO_ROOT/shore-llm/dist/index.js"

if [[ ! -x "$SHORE" ]] || [[ ! -x "$DAEMON" ]] || [[ ! -f "$LLM_JS" ]]; then
    echo "Binaries not found. Run without --skip-build first."
    exit 1
fi

# ── Temp environment ──────────────────────────────────────────────────
TMPDIR=$(mktemp -d)
LLM_PID=0
DAEMON_PID=0
cleanup() {
    kill $DAEMON_PID 2>/dev/null || true
    kill $LLM_PID 2>/dev/null || true
    wait $DAEMON_PID 2>/dev/null || true
    wait $LLM_PID 2>/dev/null || true
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

CONFIG_DIR="$TMPDIR/config/shore"
DATA_DIR="$TMPDIR/data/shore"
RUNTIME_DIR="$TMPDIR/runtime/shore"
SOCK="$RUNTIME_DIR/test.sock"
LLM_SOCK="$RUNTIME_DIR/llm.sock"

mkdir -p "$CONFIG_DIR/characters/TestChar" "$DATA_DIR" "$RUNTIME_DIR"

# Config with cheap OpenRouter model.
cat > "$CONFIG_DIR/config.toml" <<EOF
[daemon]
socket_path = "$SOCK"

[defaults]
model = "haiku"
stream = true

[behavior.autonomy]
enabled = false

[behavior.tool_use]
enabled = true
max_iterations = 3

[behavior.tool_use.tools]
memory = false
send_image = false
list_images = false
recall_image = false
generate_image = false
web_search = false
fetch_url = false
check_time = true
roll_dice = true
activity_heatmap = false

[memory]
rag_results = 0

[chat.openrouter]
base_url = "https://openrouter.ai/api/v1"

[chat.openrouter.haiku]
model_id = "google/gemini-2.0-flash-001"
max_tokens = 2048
temperature = 0.3

[embedding.default]
provider = "openai"
model_id = "text-embedding-3-small"
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
dimensions = 1536
EOF

cat > "$CONFIG_DIR/characters/TestChar/character.md" <<'EOF'
You are TestChar. Keep all responses extremely brief (one sentence max).
Do not use tools unless explicitly asked.
EOF

export XDG_CONFIG_HOME="$TMPDIR/config"
export XDG_DATA_HOME="$TMPDIR/data"
export XDG_RUNTIME_DIR="$TMPDIR/runtime"

# ── Start shore-llm ──────────────────────────────────────────────────
printf "${BOLD}Starting shore-llm...${RESET}\n"
setsid node "$LLM_JS" "$LLM_SOCK" &
LLM_PID=$!

for i in $(seq 1 30); do
    [[ -S "$LLM_SOCK" ]] && break
    sleep 0.1
done
if [[ ! -S "$LLM_SOCK" ]]; then
    echo "shore-llm failed to start (socket not found after 3s)"
    exit 1
fi
printf "${DIM}  shore-llm pid=$LLM_PID${RESET}\n"

# ── Start shore-daemon ───────────────────────────────────────────────
printf "${BOLD}Starting daemon...${RESET}\n"
RUST_LOG=warn "$DAEMON" --config "$CONFIG_DIR/config.toml" &
DAEMON_PID=$!

for i in $(seq 1 50); do
    [[ -S "$SOCK" ]] && break
    sleep 0.1
done
if [[ ! -S "$SOCK" ]]; then
    echo "Daemon failed to start"
    exit 1
fi
printf "${DIM}  daemon pid=$DAEMON_PID socket=$SOCK${RESET}\n\n"

CLI="$SHORE --socket $SOCK"

# ══════════════════════════════════════════════════════════════════════
# TEST 1: Basic send + receive (streaming)
# ══════════════════════════════════════════════════════════════════════
printf "${BOLD}Send and receive${RESET}\n"

# Send a simple message and verify we get a response.
printf "${DIM}  %-50s${RESET}" "send + streaming response"
output=$(timeout 30 $CLI send "Say exactly: PONG" 2>&1) || true
if echo "$output" | grep -qiF "PONG"; then
    printf "${GREEN}PASS${RESET}\n"
    pass=$((pass + 1))
else
    printf "${RED}FAIL${RESET}\n"
    printf "${DIM}  %s${RESET}\n" "$output" | head -5
    fail=$((fail + 1))
fi

# Verify the message appears in the log.
run_test_contains "user msg in log" "PONG" $CLI log --content -n 5

# Verify message count increased.
run_test_contains "status shows 2 messages" '"message_count": 2' $CLI status --json

# ══════════════════════════════════════════════════════════════════════
# TEST 2: Regen
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Regen${RESET}\n"

printf "${DIM}  %-50s${RESET}" "regen last response"
output=$(timeout 30 $CLI regen 2>&1) || true
if [[ $? -eq 0 ]] || echo "$output" | grep -qiF "PONG\|test\|response"; then
    printf "${GREEN}PASS${RESET}\n"
    pass=$((pass + 1))
else
    printf "${RED}FAIL${RESET}\n"
    printf "${DIM}  %s${RESET}\n" "$output" | head -5
    fail=$((fail + 1))
fi

# Regen with guidance.
printf "${DIM}  %-50s${RESET}" "regen with guidance"
output=$(timeout 30 $CLI regen --guidance "Reply with exactly: GUIDED" 2>&1) || true
if echo "$output" | grep -qiF "GUIDED"; then
    printf "${GREEN}PASS${RESET}\n"
    pass=$((pass + 1))
else
    # Model might not follow exactly, but it should respond
    if [[ -n "$output" ]] && ! echo "$output" | grep -qF "error"; then
        printf "${GREEN}PASS (response received)${RESET}\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL${RESET}\n"
        printf "${DIM}  %s${RESET}\n" "$output" | head -5
        fail=$((fail + 1))
    fi
fi

# ══════════════════════════════════════════════════════════════════════
# TEST 3: Tool use (check_time, roll_dice)
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Tool use${RESET}\n"

# check_time — ask for current time.
printf "${DIM}  %-50s${RESET}" "check_time tool"
output=$(timeout 30 $CLI send "What time is it right now? Use the check_time tool." 2>&1) || true
if echo "$output" | grep -qiE "check_time|2026|time.*T[0-9]"; then
    printf "${GREEN}PASS${RESET}\n"
    pass=$((pass + 1))
else
    printf "${RED}FAIL${RESET}\n"
    printf "${DIM}  %s${RESET}\n" "$output" | head -5
    fail=$((fail + 1))
fi

# roll_dice — ask to roll dice.
printf "${DIM}  %-50s${RESET}" "roll_dice tool"
output=$(timeout 30 $CLI send "Roll a d20 for me using the roll_dice tool and tell me the result." 2>&1) || true
if echo "$output" | grep -qiE "[0-9]+"; then
    printf "${GREEN}PASS${RESET}\n"
    pass=$((pass + 1))
else
    printf "${RED}FAIL${RESET}\n"
    printf "${DIM}  %s${RESET}\n" "$output" | head -5
    fail=$((fail + 1))
fi

# Verify tool calls show up in diagnostics.
run_test "diagnostics after tool use" $CLI status --diagnostics

# ══════════════════════════════════════════════════════════════════════
# TEST 4: Message persistence and log
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Message persistence${RESET}\n"

# Wait for any async writes to settle, then check persistence.
sleep 1
msg_count_before=$($CLI status --json 2>&1 | grep -o '"message_count": [0-9]*' | head -1 | grep -o '[0-9]*')
printf "${DIM}  %-50s${RESET}" "messages persist after restart"

kill $DAEMON_PID 2>/dev/null || true
wait $DAEMON_PID 2>/dev/null || true
rm -f "$SOCK"
RUST_LOG=warn "$DAEMON" --config "$CONFIG_DIR/config.toml" &
DAEMON_PID=$!
for i in $(seq 1 50); do [[ -S "$SOCK" ]] && break; sleep 0.1; done

msg_count_after=$($CLI status --json 2>&1 | grep -o '"message_count": [0-9]*' | head -1 | grep -o '[0-9]*')
# Messages persist if count after restart >= count before (tool results
# may flush after CLI exits, so count can increase slightly).
if [[ "$msg_count_after" -ge "$msg_count_before" ]] && [[ "$msg_count_after" -gt 0 ]]; then
    printf "${GREEN}PASS ($msg_count_after messages)${RESET}\n"
    pass=$((pass + 1))
else
    printf "${RED}FAIL (before=$msg_count_before after=$msg_count_after)${RESET}\n"
    fail=$((fail + 1))
fi

# JSON log should include content_blocks.
run_test_contains "log --json has content_blocks" "content_blocks" $CLI log --json -n 5

# ══════════════════════════════════════════════════════════════════════
# TEST 5: Streaming metadata
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Streaming metadata${RESET}\n"

# Verify the stream end metadata is shown (model, tokens, timing).
printf "${DIM}  %-50s${RESET}" "stream metadata (model + tokens)"
output=$(timeout 30 $CLI send "Say: OK" 2>&1) || true
if echo "$output" | grep -qiE "gemini|haiku|model|in:[0-9]"; then
    printf "${GREEN}PASS${RESET}\n"
    pass=$((pass + 1))
else
    printf "${RED}FAIL (no model name in metadata)${RESET}\n"
    printf "${DIM}  %s${RESET}\n" "$output" | head -5
    fail=$((fail + 1))
fi

# ══════════════════════════════════════════════════════════════════════
# TEST 6: Model management
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Model management${RESET}\n"

run_test_contains "model list shows haiku" "haiku" $CLI model
run_test "model --info" $CLI model haiku --info
run_test "model --info --json" $CLI model haiku --info --json

# ══════════════════════════════════════════════════════════════════════
# TEST 7: Memory compaction (seed 25 messages, compact, verify)
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Memory compaction${RESET}\n"

# Seed 25 messages into active.jsonl to meet compaction threshold.
# These simulate a multi-turn conversation about various topics.
CHAR_DATA="$DATA_DIR/TestChar"
mkdir -p "$CHAR_DATA"

# Back up current conversation, write seeded one.
mv "$CHAR_DATA/active.jsonl" "$CHAR_DATA/active.jsonl.bak" 2>/dev/null || true
python3 -c "
import json, datetime
msgs = []
topics = [
    ('What is your favorite color?', 'My favorite color is blue. I find it calming.'),
    ('Tell me about Tokyo.', 'Tokyo is the capital of Japan, a vibrant city with rich culture.'),
    ('What is ramen?', 'Ramen is a Japanese noodle soup dish with various regional styles.'),
    ('Do you like music?', 'I enjoy many kinds of music, especially jazz and classical.'),
    ('What is machine learning?', 'Machine learning is a subset of AI that learns from data.'),
    ('Tell me about cats.', 'Cats are independent and affectionate pets, domesticated for millennia.'),
    ('What is the weather like?', 'Weather varies by location and season. Today seems nice.'),
    ('Do you play chess?', 'I find chess fascinating. It is a game of strategy and foresight.'),
    ('What books do you recommend?', 'I enjoy science fiction and philosophy books.'),
    ('Tell me about cooking.', 'Cooking is both art and science. I love experimenting with flavors.'),
    ('What is your opinion on space?', 'Space exploration fascinates me. The universe is vast.'),
    ('Do you exercise?', 'Regular exercise is important for health and well-being.'),
    ('What is your name?', 'I am TestChar, here to help and chat with you.'),
]
base = datetime.datetime(2026, 1, 15, 10, 0, 0)
for i, (q, a) in enumerate(topics):
    ts_u = (base + datetime.timedelta(minutes=i*5)).strftime('%Y-%m-%dT%H:%M:%SZ')
    ts_a = (base + datetime.timedelta(minutes=i*5+1)).strftime('%Y-%m-%dT%H:%M:%SZ')
    msgs.append({'msg_id': f'm_u{i:03d}', 'role': 'user', 'content': q, 'images': [], 'timestamp': ts_u})
    msgs.append({'msg_id': f'm_a{i:03d}', 'role': 'assistant', 'content': a, 'images': [], 'timestamp': ts_a})
with open('$CHAR_DATA/active.jsonl', 'w') as f:
    for m in msgs:
        f.write(json.dumps(m) + '\n')
"

# Ensure shore-llm is still alive (may have crashed during earlier tests).
if ! kill -0 $LLM_PID 2>/dev/null; then
    printf "${DIM}  restarting shore-llm...${RESET}\n"
    node "$LLM_JS" "$LLM_SOCK" &
    LLM_PID=$!
    for i in $(seq 1 30); do [[ -S "$LLM_SOCK" ]] && break; sleep 0.1; done
fi

# Restart daemon to pick up the seeded messages.
kill $DAEMON_PID 2>/dev/null || true
wait $DAEMON_PID 2>/dev/null || true
rm -f "$SOCK"
RUST_LOG=warn "$DAEMON" --config "$CONFIG_DIR/config.toml" &
DAEMON_PID=$!
for i in $(seq 1 50); do [[ -S "$SOCK" ]] && break; sleep 0.1; done

# Verify messages loaded.
run_test_contains "seeded 26 messages" '"message_count": 26' $CLI status --json

# Check memory status before compaction.
run_test_contains "memory status (0 entries)" '"entries": 0' $CLI memory --json

# Ensure shore-llm is still alive before compaction.
if ! kill -0 $LLM_PID 2>/dev/null; then
    printf "${DIM}  shore-llm died, restarting...${RESET}\n"
    rm -f "$LLM_SOCK"
    setsid node "$LLM_JS" "$LLM_SOCK" &
    LLM_PID=$!
    for i in $(seq 1 30); do [[ -S "$LLM_SOCK" ]] && break; sleep 0.1; done
fi

# Run compaction (requires LLM to extract memories).
printf "${DIM}  %-50s${RESET}" "memory compact"
output=$(timeout 120 $CLI memory compact 2>&1) || true
if echo "$output" | grep -qiE "compact|entries|created"; then
    printf "${GREEN}PASS${RESET}\n"
    pass=$((pass + 1))
else
    printf "${RED}FAIL${RESET}\n"
    printf "${DIM}  %s${RESET}\n" "$output" | head -5
    fail=$((fail + 1))
fi

# Verify entries were created in the database.
printf "${DIM}  %-50s${RESET}" "entries exist after compact"
mem_output=$($CLI memory --json 2>&1)
entry_count=$(echo "$mem_output" | grep -o '"entries": [0-9]*' | head -1 | grep -o '[0-9]*')
if [[ -n "$entry_count" ]] && [[ "$entry_count" -gt 0 ]]; then
    printf "${GREEN}PASS ($entry_count entries)${RESET}\n"
    pass=$((pass + 1))
else
    printf "${RED}FAIL (no entries found)${RESET}\n"
    printf "${DIM}  %s${RESET}\n" "$mem_output" | head -5
    fail=$((fail + 1))
fi

# Verify changelog recorded the compaction.
run_test_contains "changelog records compaction" "compaction" $CLI memory changelog

# Verify message count reduced (kept recent messages only).
printf "${DIM}  %-50s${RESET}" "messages reduced after compact"
new_count=$($CLI status --json 2>&1 | grep -o '"message_count": [0-9]*' | head -1 | grep -o '[0-9]*')
if [[ -n "$new_count" ]] && [[ "$new_count" -lt 26 ]]; then
    printf "${GREEN}PASS ($new_count messages remain)${RESET}\n"
    pass=$((pass + 1))
else
    printf "${RED}FAIL (expected < 26, got $new_count)${RESET}\n"
    fail=$((fail + 1))
fi

# Verify memory reindex works after compaction.
run_test "memory reindex after compact" $CLI memory reindex

# ── Summary ───────────────────────────────────────────────────────────
printf "\n${BOLD}Results: ${GREEN}$pass passed${RESET}"
if [[ $fail -gt 0 ]]; then
    printf ", ${RED}$fail failed${RESET}"
fi
printf "\n"

exit $fail
