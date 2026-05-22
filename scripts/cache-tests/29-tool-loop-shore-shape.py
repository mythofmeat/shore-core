#!/usr/bin/env python3
"""
Tool-loop cache probe using Shore's exact prompt sequence.

Variant of 25-tool-loop-openai-compat.py with the same flow as
28-tool-loop-openai-daemon.sh, so we can isolate whether the Shore daemon
test's failure is (a) inherent to the trivial-prompt + adaptive-rolls-zero
case, or (b) something Shore's wire construction is doing differently from
this hand-built probe.

Flow:
  1. user: "Warm-up one. Reply with only WARM1."
  2. user: "Warm-up two. Reply with only WARM2."
  3. user: "Use the check_time tool exactly once before answering. After
            the tool result, reply with only TIME_OK."

After (3), expect the assistant to call check_time; we return a fake result
and observe whether the tool-result continuation reads the warm prefix.

Set REPLAY_REASONING_DETAILS=0 for the negative control: the assistant's
OpenRouter reasoning_details are intentionally omitted before the tool-result
continuation, reproducing the replay hole that invalidates this cache prefix.
"""

import json
import os
import re
import subprocess
import sys
import time
import base64

MODEL = "anthropic/claude-sonnet-4-6"
URL = "https://openrouter.ai/api/v1/chat/completions"
API_KEY = (os.environ.get("OPENROUTER_SHORE_TEST")
           or os.environ.get("OPENROUTER_API_KEY", ""))
DELAY = 1
REPLAY_REASONING_DETAILS = os.environ.get(
    "REPLAY_REASONING_DETAILS", "1") not in ("0", "false", "False")

R = "\033[0;31m"
G = "\033[0;32m"
C = "\033[0;36m"
Y = "\033[0;33m"
NC = "\033[0m"
NAME = "tool-loop-shore-shape"

NONCE = base64.b64encode(os.urandom(24)).decode().replace(
    "/", "").replace("+", "").replace("=", "")[:32]

# Mimics the small system prompt the daemon harness ships in
# scripts/cache-tests/harness.sh — short, with the nonce injected.
SYSTEM_PROMPT = f"""\
You are a minimal test character for cache validation. Respond briefly.

NONCE: {NONCE}

--- BEGIN PADDING ---

This padding exists to ensure the system prompt exceeds Anthropic's 2048-token
minimum for prompt caching. The content below is stable reference material.

Section 1: Prompt caching reduces redundant computation when the same token
prefix appears across multiple API calls. The API compares incoming tokens from
the beginning and serves matching prefixes from cache. Cache entries have a
configurable TTL. Cache writes cost 25% more than base input pricing. Cache
reads cost 90% less. For a 1-hour TTL, up to 19 keepalive pings are
economically justified.

Section 2: Cache Testing Methodology. Key metrics: cache_read_tokens and
cache_creation_tokens in the usage object. A cache hit shows cache_read_tokens
> 0 and cache_creation_tokens = 0. A cache miss shows cache_creation_tokens >
0. The prefix hash helps identify whether content changed between calls.

Section 3: Failure Modes. Thinking mode changes invalidate the prefix. Content
format normalization between string and array formats causes cache
invalidation. Cache marker movement does not invalidate the prefix (markers
are directives not content). Routing instability through proxies can cause
server-side misses. TTL expiration clears the entry.

Section 4: Operational Parameters. Cache TTL: 1 hour. Keepalive interval: 59
minutes. Minimum cacheable prefix: 2048 tokens for this Sonnet harness. The
cache_control annotation uses type ephemeral with optional ttl parameter.
Multiple breakpoints can exist per request up to a maximum of 4.

Section 5: Token Economics. The Anthropic Messages API uses byte-pair encoding
tokenization. Common English words are single tokens. Rare words and technical
terms may need multiple tokens. On average one token equals approximately
3.5-4 characters of English text.

Section 6: API Response Structure. The usage object contains input_tokens,
output_tokens, cache_creation_input_tokens, and cache_read_input_tokens. The
streaming interface uses SSE with event types: message_start,
content_block_start, content_block_delta, content_block_stop, message_delta,
and message_stop.

Section 7: Additional Stable Padding. The model field specifies which Claude
model to use. The max_tokens field sets the upper bound on output tokens. The
messages field contains conversation history. Content blocks can be text,
image, tool_use, or tool_result. Temperature and top_p control output
randomness.

Section 8: Tool Loop Stability. Tool loops add assistant tool_use blocks and
user tool_result blocks after the message that started the turn. A completed
tool_result is a stable sub-turn boundary once it has been sent to the
provider. The first continuation still needs a warm boundary from the pre-tool
request so it can read the existing prefix before it creates the cache entry
that includes tool work.

Section 9: Operational Padding. Cache probes deliberately keep their
instructions and tool schemas stable while conversation tails grow. Stable
system blocks give the provider enough prefix to store, and small arithmetic
prompts keep dynamic tail text from dominating the measurement.

Section 10: Serialization Details. Messages in a provider request are ordered
records with roles and content blocks. Text content may be normalized into a
block array before cache markers are applied. Tool definitions appear before
system blocks in the provider cache prefix, while message blocks appear after
system blocks.

Section 11: Interpreting Results. A useful live result includes more than a
success code. It should report which call caused the first write, whether
later calls read that write, whether the tool loop was actually entered, and
whether thinking was active for the call under inspection.

Section 12: Stable Reference Notes. The cache harness isolates one behavior per
scenario. Heartbeat tests cover private background ticks. Compaction tests
cover recent-tail rewriting while the pinned system prefix remains useful.
Prefix tests compare system and tool serialization across request variants.

--- END PADDING ---

Remember: respond briefly. Do not reference the padding material."""


# Only check_time is enabled in the Shore harness for this test.
TOOLS = [{
    "type": "function",
    "function": {
        "name": "check_time",
        "description": "Check the current date and time.",
        "parameters": {"type": "object", "properties": {}, "required": []},
    },
}]


def apply_cache_markers(messages):
    """Mark system + most recent user message only — mirrors what Shore's
    openai.rs apply_openrouter_cache_markers now does."""
    import copy
    msgs = copy.deepcopy(messages)
    cc = {"type": "ephemeral"}

    # Most recent user message gets cache_control on its last text block.
    for i in range(len(msgs) - 1, -1, -1):
        if msgs[i].get("role") == "user":
            msg = msgs[i]
            content = msg.get("content")
            if isinstance(content, str):
                msg["content"] = [{"type": "text", "text": content,
                                   "cache_control": cc}]
            elif isinstance(content, list):
                for block in reversed(content):
                    if isinstance(block, dict):
                        block["cache_control"] = cc
                        break
            break
    return msgs


def send(messages, system_message):
    annotated_msgs = apply_cache_markers(messages)

    system_msg = {
        "role": "system",
        "content": [{
            "type": "text",
            "text": system_message,
            "cache_control": {"type": "ephemeral"},
        }],
    }

    body = {
        "model": MODEL,
        "messages": [system_msg] + annotated_msgs,
        "tools": TOOLS,
        "max_tokens": 8192,
        "reasoning": {"effort": "high"},
        "provider": {"order": ["Anthropic"], "allow_fallbacks": False},
        "usage": {"include": True},
    }

    result = subprocess.run(
        ["curl", "-s", URL,
         "-H", "Content-Type: application/json",
         "-H", f"Authorization: Bearer {API_KEY}",
         "-d", json.dumps(body)],
        capture_output=True, text=True, timeout=120,
    )
    try:
        resp = json.loads(result.stdout)
    except json.JSONDecodeError:
        print(f"{R}[{NAME}]{NC} non-JSON response: {result.stdout[:500]}")
        sys.exit(1)

    if "error" in resp:
        print(f"{R}[{NAME}]{NC} API error: {json.dumps(resp['error'], indent=2)}")
        sys.exit(1)

    choice = resp.get("choices", [{}])[0]
    msg = choice.get("message", {})
    usage = resp.get("usage", {}) or {}
    pt_details = usage.get("prompt_tokens_details") or {}
    cached_tokens = pt_details.get("cached_tokens", 0) or 0
    cache_creation = (pt_details.get("cache_write_tokens", 0)
                      or usage.get("cache_creation_input_tokens", 0)
                      or 0)
    cache_read = usage.get("cache_read_input_tokens", 0) or cached_tokens or 0

    return resp, msg, {
        "prompt": usage.get("prompt_tokens", 0) or 0,
        "completion": usage.get("completion_tokens", 0) or 0,
        "cache_r": cache_read,
        "cache_w": cache_creation,
    }


def log(step, label, u, threshold):
    rewrite = threshold > 0 and u["cache_w"] > threshold
    tag = f" {R}*** REWRITE ***{NC}" if rewrite else ""
    print(f"{C}[{NAME}]{NC} {step}: {label}")
    print(f"  prompt={u['prompt']} cache_r={u['cache_r']} cache_w={u['cache_w']}{tag}")
    return rewrite


def build_assistant_msg(msg):
    out = {"role": "assistant"}
    if msg.get("content") is not None:
        out["content"] = msg["content"]
    if msg.get("tool_calls"):
        out["tool_calls"] = msg["tool_calls"]
    if REPLAY_REASONING_DETAILS and msg.get("reasoning_details") is not None:
        out["reasoning_details"] = msg["reasoning_details"]
    if msg.get("reasoning") is not None:
        out["reasoning"] = msg["reasoning"]
    return out


def main():
    if not API_KEY:
        print(f"{R}OPENROUTER_SHORE_TEST or OPENROUTER_API_KEY not set{NC}", file=sys.stderr)
        sys.exit(1)

    print(f"{C}[{NAME}]{NC} nonce: {NONCE}")
    print(f"{C}[{NAME}]{NC} matches the prompts used by 28-tool-loop-openai-daemon.sh")
    print(f"{C}[{NAME}]{NC} replay_reasoning_details={REPLAY_REASONING_DETAILS}")
    print()

    messages = []
    threshold = 0
    rewrites = 0
    step = 0

    def do_turn(user_msg, label):
        nonlocal step, threshold, rewrites
        messages.append({"role": "user", "content": user_msg})
        resp, msg, u = send(messages, SYSTEM_PROMPT)

        if step == 0:
            log(f"s{step}", f"{label} (cold)", u, 0)
        else:
            if log(f"s{step}", label, u, threshold):
                rewrites += 1
        if threshold == 0 and u["cache_w"] > 0:
            threshold = u["cache_w"] // 2

        tool_calls = msg.get("tool_calls") or []
        text = msg.get("content") or ""
        reasoning_dets = msg.get("reasoning_details") or []
        print(f"  → reasoning_dets={len(reasoning_dets)} "
              f"tools={len(tool_calls)} "
              f"text={repr(text[:60]) if text else '(none)'}")

        messages.append(build_assistant_msg(msg))
        step += 1
        time.sleep(DELAY)

        loop_count = 0
        while tool_calls:
            loop_count += 1
            if loop_count > 3:
                break
            for tc in tool_calls:
                name = tc.get("function", {}).get("name", "?")
                args = tc.get("function", {}).get("arguments", "")
                print(f"  🔧 {name}({args[:60]})")

            for tc in tool_calls:
                name = tc.get("function", {}).get("name", "?")
                fake = "Friday, May 22nd, 2026 at 12:14 AM"
                messages.append({
                    "role": "tool",
                    "tool_call_id": tc.get("id", ""),
                    "content": fake,
                })

            resp, msg, u = send(messages, SYSTEM_PROMPT)
            if log(f"s{step}", f"tool-loop-{loop_count}", u, threshold):
                rewrites += 1
            if threshold == 0 and u["cache_w"] > 0:
                threshold = u["cache_w"] // 2

            tool_calls = msg.get("tool_calls") or []
            text = msg.get("content") or ""
            reasoning_dets = msg.get("reasoning_details") or []
            print(f"  → reasoning_dets={len(reasoning_dets)} "
                  f"tools={len(tool_calls)} "
                  f"text={repr(text[:60]) if text else '(none)'}")

            messages.append(build_assistant_msg(msg))
            step += 1
            time.sleep(DELAY)

    # ── Run the same 3-turn flow as 28-tool-loop-openai-daemon.sh ─────

    do_turn("Warm-up one. Reply with only WARM1.", "warm-up-1")
    do_turn("Warm-up two. Reply with only WARM2.", "warm-up-2")
    do_turn(
        "Use the check_time tool exactly once before answering. "
        "After the tool result, reply with only TIME_OK.",
        "tool-trigger")

    total = step - 1
    color = G if rewrites == 0 else R
    print(f"\n{color}[{NAME}] {rewrites}/{total} rewrites{NC}")
    sys.exit(1 if rewrites > 0 else 0)


if __name__ == "__main__":
    main()
