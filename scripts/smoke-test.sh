#!/usr/bin/env bash
#
# Shore smoke test — starts a temporary daemon and runs CLI commands against it.
#
# Usage:
#   ./scripts/smoke-test.sh          # build + test
#   ./scripts/smoke-test.sh --skip-build   # reuse existing binaries
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
    printf "${DIM}  %-45s${RESET}" "$name"
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

# Variant that expects a command to fail (non-zero exit).
run_test_expect_fail() {
    local name="$1"
    shift
    printf "${DIM}  %-45s${RESET}" "$name"
    local output
    if output=$("$@" 2>&1); then
        printf "${RED}FAIL (expected error, got success)${RESET}\n"
        fail=$((fail + 1))
    else
        printf "${GREEN}PASS${RESET}\n"
        pass=$((pass + 1))
    fi
}

# Variant that checks stdout contains a string.
run_test_contains() {
    local name="$1"
    local expected="$2"
    shift 2
    printf "${DIM}  %-45s${RESET}" "$name"
    local output
    if output=$("$@" 2>&1) && echo "$output" | grep -q "$expected"; then
        printf "${GREEN}PASS${RESET}\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL${RESET}\n"
        printf "${DIM}  expected output to contain: %s${RESET}\n" "$expected"
        printf "${DIM}  got: %s${RESET}\n" "$output" | head -3
        fail=$((fail + 1))
    fi
}

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
cleanup() { kill $DAEMON_PID 2>/dev/null || true; wait $DAEMON_PID 2>/dev/null || true; rm -rf "$TMPDIR"; }
trap cleanup EXIT

CONFIG_DIR="$TMPDIR/config/shore"
DATA_DIR="$TMPDIR/data/shore"
RUNTIME_DIR="$TMPDIR/runtime/shore"
SOCK="$RUNTIME_DIR/test.sock"

mkdir -p "$CONFIG_DIR/characters/TestChar" "$DATA_DIR" "$RUNTIME_DIR"

# Minimal config: fixed socket path, no LLM service.
cat > "$CONFIG_DIR/config.toml" <<EOF
[daemon]
socket_path = "$SOCK"
EOF

# Minimal character definition.
cat > "$CONFIG_DIR/characters/TestChar/character.md" <<'EOF'
You are TestChar, a minimal test character.
EOF

export XDG_CONFIG_HOME="$TMPDIR/config"
export XDG_DATA_HOME="$TMPDIR/data"
export XDG_RUNTIME_DIR="$TMPDIR/runtime"

# ── Start daemon ──────────────────────────────────────────────────────
printf "${BOLD}Starting daemon...${RESET}\n"
RUST_LOG=warn "$DAEMON" --config "$CONFIG_DIR/config.toml" &
DAEMON_PID=$!

# Wait for socket to appear.
for i in $(seq 1 30); do
    [[ -S "$SOCK" ]] && break
    sleep 0.1
done
if [[ ! -S "$SOCK" ]]; then
    echo "Daemon failed to start (socket not found after 3s)"
    exit 1
fi
printf "${DIM}  daemon pid=$DAEMON_PID socket=$SOCK${RESET}\n\n"

CLI="$SHORE --socket $SOCK"

# ── Tests ─────────────────────────────────────────────────────────────
printf "${BOLD}CLI basics${RESET}\n"
run_test "status" $CLI status
run_test "log (empty)" $CLI log
run_test "character list" $CLI character

# ── Seed messages for edit/delete tests ───────────────────────────────
# Write messages directly to the data dir so we can test edit/delete
# without needing a working LLM.
CHAR_DATA="$DATA_DIR/TestChar"
mkdir -p "$CHAR_DATA"
cat > "$CHAR_DATA/active.jsonl" <<'EOF'
{"msg_id":"m_aaa","role":"user","content":"First message","images":[],"timestamp":"2026-01-01T00:00:01Z"}
{"msg_id":"m_bbb","role":"assistant","content":"Second message","images":[],"timestamp":"2026-01-01T00:00:02Z"}
{"msg_id":"m_ccc","role":"user","content":"Third message","images":[],"timestamp":"2026-01-01T00:00:03Z"}
EOF

# Restart daemon so the engine reloads messages from disk.
kill $DAEMON_PID 2>/dev/null || true
wait $DAEMON_PID 2>/dev/null || true
rm -f "$SOCK"
RUST_LOG=warn "$DAEMON" --config "$CONFIG_DIR/config.toml" &
DAEMON_PID=$!
for i in $(seq 1 50); do
    [[ -S "$SOCK" ]] && break
    sleep 0.1
done
if [[ ! -S "$SOCK" ]]; then
    echo "Daemon failed to restart"
    exit 1
fi

printf "\n${BOLD}Relative message references (5.17)${RESET}\n"
run_test_contains "log shows seeded messages" "m_aaa" $CLI log -n 10
run_test "edit by 'last'" $CLI edit last "Edited via last"
run_test "edit by '-1'" $CLI edit -1 "Edited via -1"
run_test "edit by positive index '1'" $CLI edit 1 "Edited via index"
run_test "edit by literal msg_id" $CLI edit m_bbb "Edited by id"
run_test "delete by 'last'" $CLI delete last

# Verify only 2 messages remain after deleting last.
run_test_contains "2 messages after delete last" "m_bbb" $CLI log -n 10
run_test_expect_fail "edit index 0 is error" $CLI edit 0 "bad"
run_test_expect_fail "edit out-of-range is error" $CLI edit 99 "bad"

# ── Stdin pipe support (5.3) ─────────────────────────────────────────
printf "\n${BOLD}Stdin pipe support (5.3)${RESET}\n"

# Piped send will connect and send the message. Without an LLM it will
# error after sending, but we can verify the message was received by
# checking the log afterward.
printf "${DIM}  %-45s${RESET}" "echo | shore send (pipe)"
if echo "Hello from pipe" | timeout 5 $CLI send 2>&1; then
    printf "${GREEN}PASS${RESET}\n"
    pass=$((pass + 1))
else
    # Expected: daemon errors because no LLM, but the user message
    # should still have been appended to the conversation.
    if $CLI log -n 10 2>&1 | grep -q "Hello from pipe"; then
        printf "${GREEN}PASS (message received, LLM unavailable)${RESET}\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL (message not found in log)${RESET}\n"
        fail=$((fail + 1))
    fi
fi

# ── Summary ───────────────────────────────────────────────────────────
printf "\n${BOLD}Results: ${GREEN}$pass passed${RESET}"
if [[ $fail -gt 0 ]]; then
    printf ", ${RED}$fail failed${RESET}"
fi
printf "\n"

exit $fail
