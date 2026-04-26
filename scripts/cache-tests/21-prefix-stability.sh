#!/usr/bin/env bash
#
# Test: verify the serialized prefix (system + tools) is byte-identical
# between normal messages and heartbeat ticks.
#
# This catches any code that mutates the system blocks, tool definitions,
# or tool ordering between request types. If the prefix changes, the
# Anthropic prompt cache is invalidated.
#
# Reads the per-call JSON files under debug/api_logs/ after running
# messages + a tick, then compares the system and tools sections of the
# request payloads.
#
set -euo pipefail
source "$(dirname "$0")/harness.sh"

harness_init "prefix-stability"

CACHE_DEPTH_TURNS="[1, 2]"
CACHE_PINNED_POSITION="[-1]"
REASONING_EFFORT="high"
OPENROUTER_PROVIDER='{ order = ["Anthropic"], allow_fallbacks = false }'

# Override config to enable autonomy + payload logging.
_write_config() {
    cat > "$CONFIG_DIR/config.toml" << TOML
[defaults]
display_name = "tester"
model        = "chat.test.model"

[behavior.autonomy]
enabled = true

[behavior.autonomy.heartbeat]
enabled = true
fallback_heartbeat_interval = "1h"
dormant_after_heartbeat_turns = 5
dormant_after_idle_time = "48h"

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
    } > "$model_toml"
}

mkdir -p "$CONFIG_DIR/characters/$CHARACTER_NAME/workspace/memory"
cat > "$CONFIG_DIR/characters/$CHARACTER_NAME/workspace/memory/MEMORY.md" << 'RECAP'
# Memory Index

The user has been asking math questions. The conversation is casual.
RECAP

harness_start

# ── Helpers ───────────────────────────────────────────────────────
send_cmd() {
    local cmd="$1"
    echo -e "${CYAN}[$TEST_NAME]${NC} cmd: $cmd"
    SHORE_CONFIG_DIR="$CONFIG_DIR" \
    SHORE_DATA_DIR="$DATA_DIR" \
        "$SHORE_BIN" --addr "$DAEMON_ADDR" \
            --character "$CHARACTER_NAME" \
            debug "$cmd" 2>>"$LOG_FILE"
}

forensics_count() {
    local path; path="$(forensics_path)"
    [[ -f "$path" ]] || { echo 0; return; }
    grep -c '"type":"response"' "$path" 2>/dev/null || echo 0
}

wait_for_tick() {
    local path; path="$(forensics_path)"
    local before; before="$(forensics_count)"
    echo -e "${CYAN}[$TEST_NAME]${NC} waiting for tick..."
    local tries=0
    while [[ $tries -lt 30 ]]; do
        sleep 2; tries=$((tries + 1))
        local now; now="$(forensics_count)"
        if [[ "$now" -gt "$before" ]]; then
            echo -e "${CYAN}[$TEST_NAME]${NC} tick detected (waited ${tries}x2s)"
            return 0
        fi
    done
    echo -e "${YELLOW}[$TEST_NAME]${NC} timeout"
    return 1
}

# ── Phase 1: Send messages ────────────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 1: Send messages ==="
for i in $(seq 1 5); do
    send_msg "Turn $i: what is $((RANDOM % 100)) + $((RANDOM % 100))?"
done

# ── Phase 2: Force tick ──────────────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 2: Force heartbeat tick ==="
send_cmd "heartbeat_tick_now"
wait_for_tick || harness_fail "tick did not fire"
# Give the tick's tool loop a moment to finish.
sleep 5

# ── Phase 3: Compare prefixes ────────────────────────────────────
echo -e "${CYAN}[$TEST_NAME]${NC} === PHASE 3: Compare prefixes ==="

API_LOGS_DIR="$DATA_DIR/debug/api_logs"
if [[ ! -d "$API_LOGS_DIR" ]]; then
    harness_fail "debug/api_logs/ not found at $API_LOGS_DIR"
fi

# Extract system and tools from each request payload and compare.
API_LOGS_DIR="$API_LOGS_DIR" python3 << 'PYEOF'
import json
import os
import pathlib
import sys

logs_dir = pathlib.Path(os.environ["API_LOGS_DIR"])

# Walk every {call_id}.json request file in chronological order (call_id
# prefix is a sortable timestamp). Skip the paired *_response.json files
# and anything that doesn't deserialize as a request envelope.
requests = []
for path in sorted(logs_dir.glob("*.json")):
    if path.name.endswith("_response.json"):
        continue
    try:
        with path.open() as f:
            entry = json.load(f)
    except (OSError, json.JSONDecodeError) as e:
        print(f"WARN: skipping {path.name}: {e}", file=sys.stderr)
        continue
    if entry.get("direction") != "request":
        continue
    body = entry.get("payload", {})
    if isinstance(body, str):
        body = json.loads(body)
    if "system" not in body:
        continue
    requests.append(body)

if len(requests) < 2:
    print(f"FAIL: fewer than 2 request payloads found in {logs_dir}")
    sys.exit(1)

# The prefix that must be stable: system blocks + tools.
# Serialize them deterministically for comparison.
def extract_prefix(body):
    """Extract the cache-critical prefix: system + tools."""
    system = json.dumps(body.get("system", []), sort_keys=True)
    tools = json.dumps(body.get("tools", []), sort_keys=True)
    return system, tools

baseline_sys, baseline_tools = extract_prefix(requests[0])
print(f"Baseline: system={len(baseline_sys)} chars, tools={len(baseline_tools)} chars")
print(f"Total requests to compare: {len(requests)}")

failures = 0
for i, req in enumerate(requests[1:], 1):
    req_sys, req_tools = extract_prefix(req)

    if req_sys != baseline_sys:
        print(f"FAIL: request {i} system differs from baseline")
        # Show first divergence point.
        for j, (a, b) in enumerate(zip(baseline_sys, req_sys)):
            if a != b:
                print(f"  First diff at char {j}: baseline={baseline_sys[max(0,j-20):j+20]!r}")
                print(f"  {'':>23}request ={req_sys[max(0,j-20):j+20]!r}")
                break
        failures += 1

    if req_tools != baseline_tools:
        print(f"FAIL: request {i} tools differ from baseline")
        # Count tool definitions.
        baseline_count = len(json.loads(baseline_tools))
        req_count = len(json.loads(req_tools))
        if baseline_count != req_count:
            print(f"  Tool count: baseline={baseline_count}, request={req_count}")
        else:
            # Find which tool differs.
            bt = json.loads(baseline_tools)
            rt = json.loads(req_tools)
            for k, (a, b) in enumerate(zip(bt, rt)):
                if json.dumps(a, sort_keys=True) != json.dumps(b, sort_keys=True):
                    print(f"  Tool {k} ({a.get('name','?')}) differs")
                    break
        failures += 1

if failures == 0:
    print(f"PASS: all {len(requests)} requests have identical system + tools prefix")
    sys.exit(0)
else:
    print(f"FAIL: {failures} prefix mismatches found")
    sys.exit(1)
PYEOF

PREFIX_RESULT=$?
if [[ $PREFIX_RESULT -ne 0 ]]; then
    harness_fail "prefix stability check failed"
fi

harness_pass
