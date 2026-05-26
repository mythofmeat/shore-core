#!/usr/bin/env bash
#
# Shore OpenRouter SDK parity live test.
#
# Drives the TypeScript daemon (backend/daemon-ts) through both SDK paths —
# Anthropic and OpenAI-compatible — over OpenRouter, with the same
# daemon/CLI checks for each, then verifies switching model SDKs mid-chat
# in both directions. The CLI binary stays Rust until the cutover replaces
# it; the daemon under test is TS.
#
# Requires: OPENROUTER_API_KEY, usually loaded from ~/.config/shore/.env.
#
# Usage:
#   ./scripts/live-tests/openrouter-sdk-parity.sh
#   ./scripts/live-tests/openrouter-sdk-parity.sh --skip-build
#
# Optional overrides:
#   SHORE_ENV_FILE                  default: ~/.config/shore/.env
#   SHORE_LIVE_ANTHROPIC_MODEL      default: anthropic/claude-haiku-4.5
#   SHORE_LIVE_OPENAI_MODEL         default: openai/gpt-5.4-mini
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SKIP_BUILD=false
[[ "${1:-}" == "--skip-build" ]] && SKIP_BUILD=true

RED='\033[0;31m'
GREEN='\033[0;32m'
DIM='\033[0;90m'
BOLD='\033[1m'
RESET='\033[0m'

pass=0
fail=0

ENV_FILE="${SHORE_ENV_FILE:-$HOME/.config/shore/.env}"
ANTHROPIC_ALIAS="${SHORE_LIVE_ANTHROPIC_ALIAS:-claude}"
ANTHROPIC_MODEL="${SHORE_LIVE_ANTHROPIC_MODEL:-anthropic/claude-haiku-4.5}"
OPENAI_ALIAS="${SHORE_LIVE_OPENAI_ALIAS:-gpt54mini}"
OPENAI_MODEL="${SHORE_LIVE_OPENAI_MODEL:-openai/gpt-5.4-mini}"

strip_ansi() { sed 's/\x1b\[[0-9;]*m//g'; }

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

run_check() {
    local name="$1"
    local ok="$2"
    local detail="${3:-}"
    printf "${DIM}  %-58s${RESET}" "$name"
    if [[ "$ok" == "true" ]]; then
        printf "${GREEN}PASS${RESET}"
        [[ -n "$detail" ]] && printf " ${DIM}(%s)${RESET}" "$detail"
        printf "\n"
        pass=$((pass + 1))
    else
        printf "${RED}FAIL${RESET}"
        [[ -n "$detail" ]] && printf " ${DIM}(%s)${RESET}" "$detail"
        printf "\n"
        fail=$((fail + 1))
    fi
}

run_cmd_contains() {
    local name="$1"
    local expected="$2"
    shift 2
    local output
    output=$("$@" 2>&1 | strip_ansi) || true
    if echo "$output" | grep -qF "$expected"; then
        run_check "$name" true "$expected"
    else
        run_check "$name" false "missing $expected"
        printf "${DIM}%s${RESET}\n" "$output" | head -5
    fi
}

run_cmd_regex() {
    local name="$1"
    local regex="$2"
    shift 2
    local output
    output=$("$@" 2>&1 | strip_ansi) || true
    if echo "$output" | grep -qiE "$regex"; then
        run_check "$name" true
    else
        run_check "$name" false "regex $regex"
        printf "${DIM}%s${RESET}\n" "$output" | head -5
    fi
}

json_field_equals() {
    local json_text="$1"
    local field="$2"
    local expected="$3"
    JSON_TEXT="$json_text" FIELD="$field" EXPECTED="$expected" python3 - <<'PY'
import json
import os
import sys

try:
    data = json.loads(os.environ["JSON_TEXT"])
except Exception as exc:
    print(f"invalid json: {exc}", file=sys.stderr)
    raise SystemExit(1)
value = data
for part in os.environ["FIELD"].split("."):
    if isinstance(value, dict) and part in value:
        value = value[part]
    else:
        print(f"missing field {os.environ['FIELD']}", file=sys.stderr)
        raise SystemExit(1)
if str(value) == os.environ["EXPECTED"]:
    raise SystemExit(0)
print(f"{os.environ['FIELD']}={value!r}, expected {os.environ['EXPECTED']!r}", file=sys.stderr)
raise SystemExit(1)
PY
}

json_field_number_gt() {
    local json_text="$1"
    local field="$2"
    local minimum="$3"
    JSON_TEXT="$json_text" FIELD="$field" MINIMUM="$minimum" python3 - <<'PY'
import json
import os
import sys

try:
    data = json.loads(os.environ["JSON_TEXT"])
except Exception as exc:
    print(f"invalid json: {exc}", file=sys.stderr)
    raise SystemExit(1)
value = data
for part in os.environ["FIELD"].split("."):
    if isinstance(value, dict) and part in value:
        value = value[part]
    else:
        print(f"missing field {os.environ['FIELD']}", file=sys.stderr)
        raise SystemExit(1)
try:
    number = float(value)
except Exception:
    print(f"{os.environ['FIELD']} is not numeric: {value!r}", file=sys.stderr)
    raise SystemExit(1)
if number > float(os.environ["MINIMUM"]):
    raise SystemExit(0)
print(f"{os.environ['FIELD']}={number}, expected > {os.environ['MINIMUM']}", file=sys.stderr)
raise SystemExit(1)
PY
}

ledger_call_count() {
    # The Rust daemon's in-memory diagnostics ring buffer was descoped
    # in the TS rewrite (audit #11); observability moved to the ledger.
    # Sum `call_count` across every row in `shore usage --last all`.
    local usage
    usage=$(timeout 20 "${CLI[@]}" usage --last all --json 2>&1 | strip_ansi) || {
        echo 0
        return
    }
    JSON_TEXT="$usage" python3 - <<'PY'
import json
import os

try:
    data = json.loads(os.environ["JSON_TEXT"])
except Exception:
    print(0)
    raise SystemExit(0)
total = 0
for entry in data.get("summary", []) or []:
    total += entry.get("call_count", 0) or 0
print(total)
PY
}

switch_model() {
    local alias="$1"
    local model_id="$2"
    local output
    output=$(timeout 30 "${CLI[@]}" model "$alias" --json 2>&1 | strip_ansi) || true
    if echo "$output" | grep -qF "\"model_id\": \"$model_id\""; then
        run_check "switch model -> $alias" true "$model_id"
    else
        run_check "switch model -> $alias" false "expected model_id $model_id"
        printf "${DIM}%s${RESET}\n" "$output" | head -5
    fi
}

assert_model_info() {
    local alias="$1"
    local sdk="$2"
    local model_id="$3"
    local output
    output=$(timeout 30 "${CLI[@]}" model "$alias" --info --json 2>&1 | strip_ansi) || true
    if json_field_equals "$output" "sdk" "$sdk" && json_field_equals "$output" "model_id" "$model_id"; then
        run_check "model info $alias reports $sdk SDK" true "$model_id"
    else
        run_check "model info $alias reports $sdk SDK" false
        printf "${DIM}%s${RESET}\n" "$output" | head -8
    fi
}

send_exact() {
    local name="$1"
    local token="$2"
    run_cmd_contains "$name" "$token" timeout 90 "${CLI[@]}" send "Reply with exactly this token and nothing else: $token"
}

regen_probe() {
    local name="$1"
    local token="$2"
    local output
    output=$(timeout 90 "${CLI[@]}" regen --guidance "Reply with exactly this token and nothing else: $token" 2>&1 | strip_ansi) || true
    if echo "$output" | grep -qF "$token"; then
        run_check "$name" true "honored guidance"
    elif [[ -n "$output" ]] && ! echo "$output" | grep -qiE "error|failed|timed out"; then
        run_check "$name" true "response received"
    else
        run_check "$name" false "regen failed or empty"
        printf "${DIM}%s${RESET}\n" "$output" | head -5
    fi
}

run_tool_probe() {
    local tool_name="$1"
    local prompt="$2"
    local regex="$3"
    local before after
    before=$(ledger_call_count)
    run_cmd_regex "$tool_name response" "$regex" timeout 90 "${CLI[@]}" send "$prompt"
    after=$(ledger_call_count)
    # A tool turn issues at least two provider calls (tool_use then
    # the post-tool follow-up); accept any growth as evidence the
    # ledger captured the loop.
    run_check "$tool_name ledger calls increased" \
        "$([[ "$after" -gt "$before" ]] && echo true || echo false)" \
        "before=$before after=$after"
}

assert_status_after_suite() {
    local suite="$1"
    local calls_before="$2"
    local status_json
    local calls_after
    status_json=$(timeout 20 "${CLI[@]}" status --json 2>&1 | strip_ansi) || true
    if json_field_number_gt "$status_json" "turn_count" 0; then
        run_check "$suite status has turns" true
    else
        run_check "$suite status has turns" false
        printf "${DIM}%s${RESET}\n" "$status_json" | head -5
    fi
    calls_after=$(ledger_call_count)
    run_check "$suite ledger calls increased" \
        "$([[ "$calls_after" -gt "$calls_before" ]] && echo true || echo false)" \
        "before=$calls_before after=$calls_after"
}

exercise_model() {
    local label="$1"
    local alias="$2"
    local sdk="$3"
    local model_id="$4"
    local token_base="$5"
    local calls_before

    printf "\n${BOLD}%s SDK suite${RESET}\n" "$label"
    calls_before=$(ledger_call_count)
    switch_model "$alias" "$model_id"
    assert_model_info "$alias" "$sdk" "$model_id"
    send_exact "$label streaming send" "${token_base}_PONG"
    regen_probe "$label regen" "${token_base}_REGEN"
    run_tool_probe \
        "check_time" \
        "You must call the check_time tool now. After the tool returns, answer with the ISO timestamp." \
        "check_time|[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}"
    run_tool_probe \
        "roll_dice" \
        "You must call the roll_dice tool for one d20. After the tool returns, answer in the form DICE=<number>." \
        "DICE=|roll_dice|\\b([1-9]|1[0-9]|20)\\b"
    run_cmd_contains "$label log contains send token" "${token_base}_PONG" timeout 20 "${CLI[@]}" log --content -n 12
    assert_status_after_suite "$label" "$calls_before"
}

if [[ -f "$ENV_FILE" ]]; then
    set -a
    # shellcheck disable=SC1090
    source "$ENV_FILE"
    set +a
fi

if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
    echo "OPENROUTER_API_KEY not set (looked in $ENV_FILE)."
    exit 1
fi

if [[ "$SKIP_BUILD" == false ]]; then
    printf "${BOLD}Building...${RESET}\n"
    cargo build --quiet -p shore-cli 2>&1
    (cd "$REPO_ROOT/backend/daemon-ts" && bun install --frozen-lockfile >/dev/null && bun run build >/dev/null)
fi

SHORE="$REPO_ROOT/target/debug/shore"
DAEMON="$REPO_ROOT/backend/daemon-ts/dist/shore-daemon"

if [[ ! -x "$SHORE" ]] || [[ ! -x "$DAEMON" ]]; then
    echo "Binaries not found. Run without --skip-build first."
    exit 1
fi

TMPDIR=$(mktemp -d)
DAEMON_PID=0
LOG_FILE="$TMPDIR/daemon.log"

cleanup() {
    if [[ "$DAEMON_PID" != "0" ]]; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
    if [[ $fail -gt 0 ]]; then
        printf "\n${DIM}Daemon log (last 80 lines):${RESET}\n"
        tail -80 "$LOG_FILE" 2>/dev/null || true
    fi
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

CONFIG_DIR="$TMPDIR/config/shore"
DATA_DIR="$TMPDIR/data/shore"
RUNTIME_DIR="$TMPDIR/runtime/shore"
INSTANCES="$RUNTIME_DIR/instances.json"
LISTEN_ADDR="127.0.0.1:0"
DAEMON_ADDR=""

mkdir -p "$CONFIG_DIR/characters/TestChar" "$DATA_DIR" "$RUNTIME_DIR"

cat > "$CONFIG_DIR/config.toml" <<EOF
[daemon]
addr = "$LISTEN_ADDR"

[defaults]
model = "$ANTHROPIC_ALIAS"
stream = true

[behavior.autonomy]
enabled = false

[behavior.tool_use]
enabled = true
max_iterations = 3

[behavior.tool_use.tools]
search_history = false
read = false
write = false
edit = false
list_files = false
search = false
delete = false
exec = false
send_image = false
generate_image = false
web_search = false
fetch_url = false
check_time = true
roll_dice = true
activity_heatmap = false

[chat.openrouter]
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api/v1"
max_tokens = 1024
temperature = 0.0

[chat.openrouter.$ANTHROPIC_ALIAS]
sdk = "anthropic"
model_id = "$ANTHROPIC_MODEL"

[chat.openrouter.$OPENAI_ALIAS]
sdk = "openai"
model_id = "$OPENAI_MODEL"
EOF

cat > "$CONFIG_DIR/characters/TestChar/character.md" <<'EOF'
You are TestChar, a live SDK parity test assistant.
Keep ordinary responses to one short sentence.
When the user asks you to use a named tool, you must call that tool before answering.
When the user asks for an exact token, reply with only that token.
EOF

export XDG_CONFIG_HOME="$TMPDIR/config"
export XDG_DATA_HOME="$TMPDIR/data"
export XDG_RUNTIME_DIR="$TMPDIR/runtime"
export SHORE_RUNTIME_DIR="$RUNTIME_DIR"
export NO_COLOR=1

printf "${BOLD}Starting daemon...${RESET}\n"
NO_COLOR=1 SHORE_RUNTIME_DIR="$RUNTIME_DIR" "$DAEMON" --config "$CONFIG_DIR/config.toml" > "$LOG_FILE" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 80); do
    DAEMON_ADDR="$(registered_daemon_addr "$INSTANCES" "$DAEMON_PID" 2>/dev/null || true)"
    [[ -n "$DAEMON_ADDR" ]] && break
    kill -0 "$DAEMON_PID" 2>/dev/null || break
    sleep 0.1
done

if [[ -z "$DAEMON_ADDR" ]]; then
    echo "Daemon failed to start (address not registered)."
    cat "$LOG_FILE"
    exit 1
fi

printf "${DIM}  daemon pid=%s addr=%s${RESET}\n" "$DAEMON_PID" "$DAEMON_ADDR"
printf "${DIM}  anthropic=%s openai_compatible=%s${RESET}\n\n" "$ANTHROPIC_MODEL" "$OPENAI_MODEL"

CLI=("$SHORE" "--addr" "$DAEMON_ADDR")

exercise_model "Anthropic" "$ANTHROPIC_ALIAS" "anthropic" "$ANTHROPIC_MODEL" "ANTHROPIC_SDK"
exercise_model "OpenAI-compatible" "$OPENAI_ALIAS" "openai" "$OPENAI_MODEL" "OPENAI_COMPAT_SDK"

printf "\n${BOLD}Mid-chat SDK switching${RESET}\n"
switch_model "$ANTHROPIC_ALIAS" "$ANTHROPIC_MODEL"
send_exact "prime on Anthropic before OpenAI switch" "SWITCH_A2O_ANTHROPIC"
switch_model "$OPENAI_ALIAS" "$OPENAI_MODEL"
send_exact "Anthropic -> OpenAI-compatible" "SWITCH_A2O_OPENAI"
switch_model "$ANTHROPIC_ALIAS" "$ANTHROPIC_MODEL"
send_exact "OpenAI-compatible -> Anthropic" "SWITCH_O2A_ANTHROPIC"
run_cmd_contains "log contains A2O anchor" "SWITCH_A2O_ANTHROPIC" timeout 20 "${CLI[@]}" log --content -n 12
run_cmd_contains "log contains A2O switched reply" "SWITCH_A2O_OPENAI" timeout 20 "${CLI[@]}" log --content -n 12
run_cmd_contains "log contains O2A switched reply" "SWITCH_O2A_ANTHROPIC" timeout 20 "${CLI[@]}" log --content -n 12

printf "\n${BOLD}Summary${RESET}\n"
printf "${GREEN}%s passed${RESET}" "$pass"
if [[ $fail -gt 0 ]]; then
    printf ", ${RED}%s failed${RESET}" "$fail"
fi
printf "\n"

exit "$fail"
