#!/usr/bin/env python3
"""
Curl-based test: OpenAI chat completions format to OpenRouter.

Tests whether /v1/chat/completions has more stable caching than
/v1/messages (Anthropic format, which Shore uses natively).

Mimics SillyTavern's exact request format:
  - OpenAI chat completions endpoint
  - System message inline with cache_control on last text block
  - Sliding breakpoints at depth D and D+2 role switches from end
  - HTTP-Referer and X-Title headers
  - Non-cached messages stay as strings (not array-of-blocks)
"""

import json
import os
import subprocess
import sys
import time
import random
import base64
import copy

# ── Config ─────────────────────────────────────────────────────────
MODEL = "anthropic/claude-sonnet-4-6"
URL = "https://openrouter.ai/api/v1/chat/completions"
API_KEY = os.environ.get("OPENROUTER_SHORE_TEST", "")
DELAY = 4
TURNS = 10
CACHING_DEPTH = 2  # SillyTavern default

# ── Colors ─────────────────────────────────────────────────────────
R = "\033[0;31m"
G = "\033[0;32m"
C = "\033[0;36m"
Y = "\033[0;33m"
NC = "\033[0m"
NAME = "curl-openai-fmt"

# ── System prompt (identical to harness character.md) ──────────────
NONCE = base64.b64encode(os.urandom(24)).decode().replace(
    "/", "").replace("+", "").replace("=", "")[:32]

SYSTEM_PROMPT = f"""\
You are a minimal test character for cache validation. Respond briefly.

NONCE: {NONCE}

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

Remember: respond briefly. Do not reference the padding material."""


def apply_sillytavern_caching(messages):
    """Apply cache_control exactly as SillyTavern does.

    1. System prompt: cache_control on last text block of first system msg.
    2. Sliding: count role switches backwards (skip system, skip trailing
       assistant prefill), apply at depth D and D+2.
    """
    msgs = copy.deepcopy(messages)

    # ── System prompt caching ──────────────────────────────────────
    for msg in msgs:
        if msg["role"] == "system":
            content = msg["content"]
            if isinstance(content, str):
                msg["content"] = [{
                    "type": "text",
                    "text": content,
                    "cache_control": {"type": "ephemeral"},
                }]
            elif isinstance(content, list):
                for j in range(len(content) - 1, -1, -1):
                    if content[j].get("type") == "text":
                        content[j]["cache_control"] = {"type": "ephemeral"}
                        break
            break  # Only first system message

    # ── Sliding breakpoints (SillyTavern logic) ───────────────────
    passed_prefill = False
    depth = 0
    prev_role = ""

    for i in range(len(msgs) - 1, -1, -1):
        role = msgs[i]["role"]

        # Skip trailing assistant prefill
        if not passed_prefill and role == "assistant":
            continue
        passed_prefill = True

        # Skip system messages
        if role == "system":
            continue

        if role != prev_role:
            if depth == CACHING_DEPTH or depth == CACHING_DEPTH + 2:
                content = msgs[i]["content"]
                if isinstance(content, str):
                    msgs[i]["content"] = [{
                        "type": "text",
                        "text": content,
                        "cache_control": {"type": "ephemeral"},
                    }]
                elif isinstance(content, list):
                    content[-1]["cache_control"] = {"type": "ephemeral"}

            if depth == CACHING_DEPTH + 2:
                break

            depth += 1
            prev_role = role

    return msgs


TOOLS = [
    {"type": "function", "function": {"name": "memory", "description": "Access, search, or modify the character's long-term memory about the user and past conversations. Use this to recall previous topics, store important information, or maintain continuity across sessions.", "parameters": {"type": "object", "properties": {"request": {"type": "string", "description": "Natural language request describing what to remember, recall, or search for"}}, "required": ["request"]}}},
    {"type": "function", "function": {"name": "send_image", "description": "Send an image from the character's local image library to the user. Use this when the user asks to see a photo, picture, or image that the character has.", "parameters": {"type": "object", "properties": {"path": {"type": "string", "description": "Path to the image file"}, "caption": {"type": "string", "description": "Optional caption or description to send with the image"}}, "required": ["path"]}}},
    {"type": "function", "function": {"name": "list_images", "description": "List available images in the character's image library. Returns file paths and metadata. Use this to browse what images are available before sending one.", "parameters": {"type": "object", "properties": {"query": {"type": "string", "description": "Optional search query to filter images by name or metadata"}}, "required": []}}},
    {"type": "function", "function": {"name": "recall_image", "description": "Recall and view a previously seen image from the conversation or image library. Retrieves the image data for the character to reference.", "parameters": {"type": "object", "properties": {"path": {"type": "string", "description": "Path to the image to recall"}}, "required": ["path"]}}},
    {"type": "function", "function": {"name": "remember_image", "description": "Store a description or memory about an image for future reference. Use this to remember details about images the user has shared.", "parameters": {"type": "object", "properties": {"path": {"type": "string", "description": "Path or identifier for the image"}, "description": {"type": "string", "description": "Detailed description of the image content and context"}}, "required": ["path", "description"]}}},
    {"type": "function", "function": {"name": "generate_image", "description": "Generate a new image using AI image generation. Produces an image based on a text description and sends it to the user.", "parameters": {"type": "object", "properties": {"prompt": {"type": "string", "description": "Detailed text description of the image to generate"}, "size": {"type": "string", "description": "Image dimensions", "default": "1024x1024"}}, "required": ["prompt"]}}},
    {"type": "function", "function": {"name": "web_search", "description": "Search the web for current information. Use when the user asks about recent events, facts you're unsure about, or anything that may have changed since training.", "parameters": {"type": "object", "properties": {"query": {"type": "string", "description": "Search query"}, "max_results": {"type": "integer", "description": "Maximum number of results to return", "default": 5}}, "required": ["query"]}}},
    {"type": "function", "function": {"name": "fetch_url", "description": "Fetch the content of a specific URL. Use when the user shares a link and wants you to read or summarize its content.", "parameters": {"type": "object", "properties": {"url": {"type": "string", "description": "The URL to fetch content from"}}, "required": ["url"]}}},
    {"type": "function", "function": {"name": "check_time", "description": "Check the current date and time. Use when the user asks what time it is, what day it is, or for any time-sensitive context.", "parameters": {"type": "object", "properties": {}, "required": []}}},
    {"type": "function", "function": {"name": "roll_dice", "description": "Roll dice using standard notation. Supports complex expressions like 2d6+3, 4d8, d20, etc. Use for games, random decisions, or when the user asks you to roll.", "parameters": {"type": "object", "properties": {"notation": {"type": "string", "description": "Dice notation string (e.g. '2d6+3', 'd20', '4d8')"}}, "required": ["notation"]}}},
    {"type": "function", "function": {"name": "activity_heatmap", "description": "Generate a heatmap visualization of conversation activity over time. Shows when and how often conversations happen.", "parameters": {"type": "object", "properties": {"days": {"type": "integer", "description": "Number of days to include in the heatmap", "default": 30}}, "required": []}}},
    {"type": "function", "function": {"name": "scratchpad_list", "description": "List files in the character's scratchpad. The scratchpad is a persistent workspace for notes, drafts, and working documents.", "parameters": {"type": "object", "properties": {"path": {"type": "string", "description": "Optional subdirectory path to list"}}, "required": []}}},
    {"type": "function", "function": {"name": "scratchpad_read", "description": "Read a file from the character's scratchpad. Use to retrieve previously saved notes, drafts, or working documents.", "parameters": {"type": "object", "properties": {"path": {"type": "string", "description": "Path to the file to read"}}, "required": ["path"]}}},
    {"type": "function", "function": {"name": "scratchpad_write", "description": "Write or update a file in the character's scratchpad. Use to save notes, drafts, code, or any persistent working document.", "parameters": {"type": "object", "properties": {"path": {"type": "string", "description": "Path for the file to write"}, "content": {"type": "string", "description": "Content to write to the file"}}, "required": ["path", "content"]}}},
]


def send_curl(messages):
    """Send request via curl, return parsed JSON response."""
    payload = json.dumps({
        "model": MODEL,
        "messages": messages,
        "tools": TOOLS,
        "max_tokens": 256,
    })

    result = subprocess.run(
        [
            "curl", "-s", URL,
            "-H", "Content-Type: application/json",
            "-H", f"Authorization: Bearer {API_KEY}",
            "-H", "HTTP-Referer: https://sillytavern.app",
            "-H", "X-Title: SillyTavern",
            "-d", payload,
        ],
        capture_output=True,
        text=True,
        timeout=60,
    )

    if result.returncode != 0:
        print(f"{R}[{NAME}]{NC} curl failed: {result.stderr}", file=sys.stderr)
        sys.exit(1)

    return json.loads(result.stdout)


def extract_cache_metrics(usage):
    """Extract cache metrics — OpenRouter uses different fields per format."""
    details = usage.get("prompt_tokens_details", {}) or {}

    # OpenAI-style (what /v1/chat/completions returns)
    cache_r = details.get("cached_tokens", 0) or 0
    cache_w = details.get("cache_write_tokens", 0) or 0

    # Anthropic-style fallback (what /v1/messages returns)
    if cache_w == 0 and cache_r == 0:
        cache_w = usage.get("cache_creation_input_tokens", 0) or 0
        cache_r = usage.get("cache_read_input_tokens", 0) or 0

    prompt = usage.get("prompt_tokens", 0) or usage.get("input_tokens", 0) or 0

    return prompt, cache_r, cache_w


def main():
    if not API_KEY:
        print(f"{R}[{NAME}]{NC} OPENROUTER_SHORE_TEST not set", file=sys.stderr)
        sys.exit(1)

    print(f"{C}[{NAME}]{NC} nonce: {NONCE}")
    print(f"{C}[{NAME}]{NC} endpoint: {URL}")
    print(f"{C}[{NAME}]{NC} format: OpenAI chat completions (SillyTavern-style)")

    # Conversation: system message + growing history
    system_msg = {"role": "system", "content": SYSTEM_PROMPT}
    history = []  # user/assistant messages (without system)

    first_write = 0
    threshold = 0
    failures = 0
    summary = []

    for turn in range(TURNS):
        a, b = random.randint(0, 99), random.randint(0, 99)
        user_text = f"Cache test turn {turn + 1}. What is {a} plus {b}?"
        print(f"{C}[{NAME}]{NC} send: {user_text}")

        history.append({"role": "user", "content": user_text})

        # Build full message list and apply SillyTavern-style caching
        all_msgs = [system_msg] + history
        cached_msgs = apply_sillytavern_caching(all_msgs)

        # Send via curl
        resp = send_curl(cached_msgs)

        if "error" in resp:
            print(f"{R}[{NAME}]{NC} API error: {json.dumps(resp['error'])}")
            sys.exit(1)

        # On first response, dump full usage for debugging
        usage = resp.get("usage", {})
        if turn == 0:
            print(f"{C}[{NAME}]{NC} usage fields: {json.dumps(usage, indent=2)}")

        prompt, cache_r, cache_w = extract_cache_metrics(usage)

        # Extract assistant response
        choices = resp.get("choices", [])
        assistant_text = choices[0]["message"]["content"] if choices else "(no response)"
        print(assistant_text)

        history.append({"role": "assistant", "content": assistant_text})

        # Check cache metrics
        if turn == 0:
            first_write = cache_w
            threshold = first_write // 2
            print(f"{C}[{NAME}]{NC}   turn 0: cache_w={cache_w} prompt={prompt} "
                  f"(cold start, threshold={threshold})")
        else:
            tag = ""
            if threshold > 0 and cache_w > threshold:
                tag = f" {R}*** FULL REWRITE ***{NC}"
                failures += 1
            print(f"{C}[{NAME}]{NC}   turn {turn}: cache_r={cache_r} "
                  f"cache_w={cache_w} prompt={prompt}{tag}")

        summary.append({"turn": turn, "prompt": prompt,
                        "cache_r": cache_r, "cache_w": cache_w})

        if turn < TURNS - 1:
            time.sleep(DELAY)

    # ── Summary ────────────────────────────────────────────────────
    print(f"\n{C}[{NAME}]{NC} forensics summary:")
    for s in summary:
        w_tag = "  WRITE" if s["cache_w"] > 0 else ""
        full_tag = "  *** FULL REWRITE ***" if (
            s["turn"] > 0 and threshold > 0 and s["cache_w"] > threshold
        ) else ""
        print(f"  [{s['turn']}] prompt={s['prompt']} "
              f"cache_r={s['cache_r']} cache_w={s['cache_w']}{w_tag}{full_tag}")

    if failures == 0:
        print(f"\n{G}[{NAME}] PASS — 0/{TURNS} full rewrites{NC}")
    else:
        print(f"\n{R}[{NAME}] {failures}/{TURNS} full rewrites{NC}")

    sys.exit(1 if failures > 0 else 0)


if __name__ == "__main__":
    main()
