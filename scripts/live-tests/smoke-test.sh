#!/usr/bin/env bash
#
# Shore smoke test — starts a temporary daemon and runs CLI commands against it.
#
# Usage:
#   ./scripts/live-tests/smoke-test.sh          # build + test
#   ./scripts/live-tests/smoke-test.sh --skip-build   # reuse existing binaries
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
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
        printf "${DIM}  got: %s${RESET}\n" "$output" | head -3
        fail=$((fail + 1))
    fi
}

run_test_not_contains() {
    local name="$1"
    local unexpected="$2"
    shift 2
    printf "${DIM}  %-50s${RESET}" "$name"
    local output
    if output=$("$@" 2>&1) && ! echo "$output" | grep -qF "$unexpected"; then
        printf "${GREEN}PASS${RESET}\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL${RESET}\n"
        printf "${DIM}  expected output NOT to contain: %s${RESET}\n" "$unexpected"
        fail=$((fail + 1))
    fi
}

# ── Helpers ───────────────────────────────────────────────────────────

start_daemon() {
    rm -f "$SOCK"
    RUST_LOG=warn "$DAEMON" --config "$CONFIG_DIR/config.toml" &
    DAEMON_PID=$!
    for i in $(seq 1 50); do
        [[ -S "$SOCK" ]] && break
        sleep 0.1
    done
    if [[ ! -S "$SOCK" ]]; then
        echo "Daemon failed to start (socket not found after 5s)"
        exit 1
    fi
}

restart_daemon() {
    kill $DAEMON_PID 2>/dev/null || true
    wait $DAEMON_PID 2>/dev/null || true
    start_daemon
}

clear_state() {
    rm -f "$RUNTIME_DIR/active_character"
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
DAEMON_PID=0
cleanup() { kill $DAEMON_PID 2>/dev/null || true; wait $DAEMON_PID 2>/dev/null || true; rm -rf "$TMPDIR"; }
trap cleanup EXIT

CONFIG_DIR="$TMPDIR/config/shore"
DATA_DIR="$TMPDIR/data/shore"
RUNTIME_DIR="$TMPDIR/runtime/shore"
SOCK="$RUNTIME_DIR/test.sock"

mkdir -p "$CONFIG_DIR/characters/TestChar" "$DATA_DIR" "$RUNTIME_DIR"

cat > "$CONFIG_DIR/config.toml" <<EOF
[daemon]
socket_path = "$SOCK"
EOF

cat > "$CONFIG_DIR/characters/TestChar/character.md" <<'EOF'
You are TestChar, a minimal test character.
EOF

export XDG_CONFIG_HOME="$TMPDIR/config"
export XDG_DATA_HOME="$TMPDIR/data"
export XDG_RUNTIME_DIR="$TMPDIR/runtime"

# ── Start daemon (single character — no -c needed) ────────────────────
printf "${BOLD}Starting daemon...${RESET}\n"
start_daemon
printf "${DIM}  daemon pid=$DAEMON_PID socket=$SOCK${RESET}\n\n"

CLI="$SHORE --socket $SOCK"

# ══════════════════════════════════════════════════════════════════════
# SECTION 1: CLI basics (empty state, single character)
# ══════════════════════════════════════════════════════════════════════
printf "${BOLD}CLI basics${RESET}\n"
run_test "status" $CLI status
run_test "status --json" $CLI status --json
run_test_contains "status shows character name" "TestChar" $CLI status
run_test "log (empty)" $CLI log
run_test "log --json (empty)" $CLI log --json
run_test "character list" $CLI character
run_test_contains "character list shows TestChar" "TestChar" $CLI character
run_test "model (list, no models configured)" $CLI model
run_test "config --path" $CLI config --path
run_test_contains "config --path shows shore" "shore" $CLI config --path
run_test "config (show all)" $CLI config

# ══════════════════════════════════════════════════════════════════════
# SECTION 2: Config commands
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Config commands${RESET}\n"
run_test "config --check" $CLI config --check
run_test "config --json" $CLI config --json
run_test "config set key" $CLI config autonomy.enabled false
run_test_contains "config get section" "defaults" $CLI config defaults
run_test "config --reset" $CLI config --reset

# ══════════════════════════════════════════════════════════════════════
# SECTION 3: Shell completions
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Shell completions${RESET}\n"
run_test_contains "completions fish" "shore" $CLI completions fish
run_test_contains "completions bash" "shore" $CLI completions bash
run_test_contains "completions zsh" "shore" $CLI completions zsh

# ══════════════════════════════════════════════════════════════════════
# SECTION 4: Seed messages for log/edit/delete tests
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Seeding messages...${RESET}\n"

CHAR_DATA="$DATA_DIR/TestChar"
mkdir -p "$CHAR_DATA"
cat > "$CHAR_DATA/active.jsonl" <<'EOF'
{"msg_id":"m_aaa","role":"user","content":"First message","images":[],"timestamp":"2026-01-01T00:00:01Z"}
{"msg_id":"m_bbb","role":"assistant","content":"Second message","images":[],"timestamp":"2026-01-01T00:00:02Z"}
{"msg_id":"m_ccc","role":"user","content":"Third message","images":[],"timestamp":"2026-01-01T00:00:03Z"}
EOF

restart_daemon
printf "${DIM}  daemon restarted with seeded messages${RESET}\n"

# ══════════════════════════════════════════════════════════════════════
# SECTION 5: Log viewing
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Log viewing${RESET}\n"
run_test "log shows seeded messages" $CLI log -n 10
run_test_contains "log --content shows text" "First message" $CLI log --content -n 10
run_test_contains "log --json has msg_id" "m_aaa" $CLI log --json -n 10
run_test_contains "log --json has all 3" "m_ccc" $CLI log --json -n 10

# Single message by reference (shore log <ref>).
run_test_contains "log last (json)" "Third message" $CLI log --json last
run_test_contains "log -1 (json)" "Third message" $CLI log --json -1
run_test_contains "log 1 (json, first msg)" "First message" $CLI log --json 1
run_test_contains "log 2 (json, second msg)" "Second message" $CLI log --json 2

# ══════════════════════════════════════════════════════════════════════
# SECTION 6: Message editing (shore log edit)
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Message editing${RESET}\n"
run_test_contains "edit by 'last'" "Edited" $CLI log edit last "Edited via last"
run_test_contains "edit by '-1'" "Edited" $CLI log edit -1 "Edited via -1"
run_test_contains "edit by positive index '1'" "Edited" $CLI log edit 1 "Edited via index"
run_test_contains "edit by literal msg_id" "Edited" $CLI log edit m_bbb "Edited by id"

# Verify the edit persisted.
run_test_contains "edit persisted" "Edited via -1" $CLI log --content -n 10

# Error cases.
run_test_expect_fail "edit index 0 is error" $CLI log edit 0 "bad"
run_test_expect_fail "edit out-of-range is error" $CLI log edit 99 "bad"

# ══════════════════════════════════════════════════════════════════════
# SECTION 7: Message deletion (shore log delete)
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Message deletion${RESET}\n"
run_test_contains "delete by 'last'" "Deleted" $CLI log delete last

# Verify only 2 messages remain.
run_test_not_contains "deleted msg gone from log" "Edited via -1" $CLI log --content -n 10
run_test_contains "remaining msgs present" "Edited by id" $CLI log --content -n 10

# Error: delete out of range.
run_test_expect_fail "delete out-of-range" $CLI log delete 99

# ══════════════════════════════════════════════════════════════════════
# SECTION 8: Status sections and diagnostics
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Status and diagnostics${RESET}\n"
run_test "status --json" $CLI status --json
run_test "status --diagnostics" $CLI status --diagnostics
run_test "status --diagnostics -n 5" $CLI status --diagnostics -n 5
run_test "status --diagnostics --json" $CLI status --diagnostics --json

# Invalid section.
run_test_expect_fail "status --section invalid" $CLI status --section nonexistent

# ══════════════════════════════════════════════════════════════════════
# SECTION 9: Memory commands (no LLM — structural tests)
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Memory commands${RESET}\n"
run_test "memory (status/query, no args)" $CLI memory
run_test "memory changelog" $CLI memory changelog
run_test "memory changelog -n 5" $CLI memory changelog -n 5

# ══════════════════════════════════════════════════════════════════════
# SECTION 10: Heartbeat log
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Heartbeat log${RESET}\n"
run_test "log --heartbeat" $CLI log --heartbeat

# ══════════════════════════════════════════════════════════════════════
# SECTION 11: Stdin pipe support
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Stdin pipe support${RESET}\n"

printf "${DIM}  %-50s${RESET}" "echo | shore send (pipe)"
if echo "Hello from pipe" | timeout 5 $CLI send 2>&1; then
    printf "${GREEN}PASS${RESET}\n"
    pass=$((pass + 1))
else
    # Expected: daemon errors because no LLM, but the user message
    # should still have been appended to the conversation.
    if $CLI log --content -n 10 2>&1 | grep -qF "Hello from pipe"; then
        printf "${GREEN}PASS (message received, LLM unavailable)${RESET}\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL (message not found in log)${RESET}\n"
        fail=$((fail + 1))
    fi
fi

# ══════════════════════════════════════════════════════════════════════
# SECTION 12: Global flags
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Global flags${RESET}\n"
run_test "no-color flag" $SHORE --socket "$SOCK" --no-color status
run_test_contains "NO_COLOR env" "TestChar" env NO_COLOR=1 $SHORE --socket "$SOCK" status
run_test_contains "-c flag selects character" "TestChar" $SHORE --socket "$SOCK" -c TestChar status

# ══════════════════════════════════════════════════════════════════════
# SECTION 13: Error handling
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Error handling${RESET}\n"
run_test_expect_fail "bad socket path" $SHORE --socket /tmp/nonexistent_shore_test.sock status
run_test_expect_fail "unknown command" $SHORE --socket "$SOCK" badcommand

# ══════════════════════════════════════════════════════════════════════
# SECTION 14: Character management (multi-character — tested last)
#
# Creates a second character, restarts daemon, tests switching.
# This section is last because multi-character mode requires -c flags.
# ══════════════════════════════════════════════════════════════════════
printf "\n${BOLD}Character management${RESET}\n"

# Create second character scaffold (CLI-only, no daemon needed).
run_test "character --new SecondChar" $CLI character SecondChar --new

# Restart daemon to discover the new character.
restart_daemon

# With two characters, daemon requires explicit selection.
CLIA="$SHORE --socket $SOCK -c TestChar"
CLIB="$SHORE --socket $SOCK -c SecondChar"

run_test_contains "char list includes SecondChar" "SecondChar" $CLIA character
run_test_contains "switch to SecondChar" "Switched" $CLIA character SecondChar
run_test_contains "switch back to TestChar" "Switched" $CLIB character TestChar

# Character info.
clear_state
run_test "character --info" $CLIA character TestChar --info
run_test "character --info --json" $CLIA character TestChar --info --json

# Invalid character.
clear_state
run_test_expect_fail "switch nonexistent character" $CLIA character NoSuchChar

# ── Summary ───────────────────────────────────────────────────────────
clear_state

printf "\n${BOLD}Results: ${GREEN}$pass passed${RESET}"
if [[ $fail -gt 0 ]]; then
    printf ", ${RED}$fail failed${RESET}"
fi
printf "\n"

exit $fail
