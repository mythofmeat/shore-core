#!/usr/bin/env python3
"""
Systematic cache experiment runner.

Varies: endpoint format, headers, delay, breakpoint strategy, content format.
Each experiment runs 10 turns and records per-turn cache metrics.
Results are appended to a JSONL file for later analysis.
"""

import json
import os
import subprocess
import sys
import time
import random
import base64
import copy
import argparse
from datetime import datetime

# ── System prompt (same as harness character.md) ──────────────────
def make_system_prompt(nonce):
    return f"""\
You are a minimal test character for cache validation. Respond briefly.

NONCE: {nonce}

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


# ── Tool definitions (push above 2048 token minimum) ─────────────
TOOLS_OPENAI = [
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

# Anthropic-format tools (name, description, input_schema instead of parameters)
TOOLS_ANTHROPIC = [
    {"name": t["function"]["name"],
     "description": t["function"]["description"],
     "input_schema": t["function"]["parameters"]}
    for t in TOOLS_OPENAI
]

# ── Colors ─────────────────────────────────────────────────────────
R = "\033[0;31m"
G = "\033[0;32m"
C = "\033[0;36m"
Y = "\033[0;33m"
NC = "\033[0m"


# ── Breakpoint strategies ─────────────────────────────────────────

def apply_sillytavern_caching(messages, caching_depth=2):
    """SillyTavern-style: system anchor + depth D and D+2 role switches."""
    msgs = copy.deepcopy(messages)

    # System prompt: cache_control on last text block of first system msg
    for msg in msgs:
        if msg["role"] == "system":
            content = msg["content"]
            if isinstance(content, str):
                msg["content"] = [{"type": "text", "text": content,
                                   "cache_control": {"type": "ephemeral"}}]
            elif isinstance(content, list):
                for j in range(len(content) - 1, -1, -1):
                    if content[j].get("type") == "text":
                        content[j]["cache_control"] = {"type": "ephemeral"}
                        break
            break

    # Sliding breakpoints
    passed_prefill = False
    depth = 0
    prev_role = ""
    for i in range(len(msgs) - 1, -1, -1):
        role = msgs[i]["role"]
        if not passed_prefill and role == "assistant":
            continue
        passed_prefill = True
        if role == "system":
            continue
        if role != prev_role:
            if depth == caching_depth or depth == caching_depth + 2:
                content = msgs[i]["content"]
                if isinstance(content, str):
                    msgs[i]["content"] = [{"type": "text", "text": content,
                                           "cache_control": {"type": "ephemeral"}}]
                elif isinstance(content, list):
                    content[-1]["cache_control"] = {"type": "ephemeral"}
            if depth == caching_depth + 2:
                break
            depth += 1
            prev_role = role

    return msgs


def apply_system_only_caching(messages):
    """Only a system prompt breakpoint. No sliding message breakpoints."""
    msgs = copy.deepcopy(messages)
    for msg in msgs:
        if msg["role"] == "system":
            content = msg["content"]
            if isinstance(content, str):
                msg["content"] = [{"type": "text", "text": content,
                                   "cache_control": {"type": "ephemeral"}}]
            elif isinstance(content, list):
                for j in range(len(content) - 1, -1, -1):
                    if content[j].get("type") == "text":
                        content[j]["cache_control"] = {"type": "ephemeral"}
                        break
            break
    return msgs


def apply_shore_style_caching(messages, depth_turns=None, pinned=None):
    """Shore-style breakpoints: depth_turns counts user msgs from end,
    pinned places a system breakpoint. For OpenAI format (system inline).

    depth_turns: list of ints, e.g. [1,2] or [0,1]. None = no sliding.
    pinned: int or None. 0/-1 = system msg gets cache_control. None = no pin.
    """
    msgs = copy.deepcopy(messages)

    # System breakpoint
    if pinned is not None:
        for msg in msgs:
            if msg["role"] == "system":
                content = msg["content"]
                if isinstance(content, str):
                    msg["content"] = [{"type": "text", "text": content,
                                       "cache_control": {"type": "ephemeral"}}]
                elif isinstance(content, list):
                    content[-1]["cache_control"] = {"type": "ephemeral"}
                break

    if not depth_turns:
        return msgs

    # Sliding breakpoints: count real user msgs from end
    non_system = [(i, m) for i, m in enumerate(msgs) if m["role"] != "system"]
    user_positions = [i for i, m in non_system if m["role"] == "user"]

    bp_indices = set()
    for depth in depth_turns:
        # depth=0 → last user msg's preceding assistant (or the user itself)
        # depth=1 → one user msg further back, breakpoint on msg before it
        target_user = len(user_positions) - 1 - depth
        if target_user < 0:
            continue  # not enough turns
        user_real_idx = user_positions[target_user]
        # Place BP on the message before this user msg (the assistant response)
        bp_idx = user_real_idx - 1 if user_real_idx > 0 else user_real_idx
        if bp_idx <= 0:
            continue  # skip msg[0] (system) and msg[1] (first user)
        bp_indices.add(bp_idx)

    for idx in bp_indices:
        if idx < len(msgs):
            content = msgs[idx]["content"]
            if isinstance(content, str):
                msgs[idx]["content"] = [{"type": "text", "text": content,
                                         "cache_control": {"type": "ephemeral"}}]
            elif isinstance(content, list):
                content[-1]["cache_control"] = {"type": "ephemeral"}

    return msgs


# ── Request sending ───────────────────────────────────────────────

def send_openai_format(messages, api_key, headers, tools=True,
                       provider_pin=None):
    """Send via /v1/chat/completions (OpenAI format)."""
    url = "https://openrouter.ai/api/v1/chat/completions"
    body = {
        "model": "anthropic/claude-sonnet-4-6",
        "messages": messages,
        "max_tokens": 256,
    }
    if tools:
        body["tools"] = TOOLS_OPENAI
    if provider_pin:
        body["provider"] = {"order": [provider_pin], "allow_fallbacks": False}

    curl_headers = [
        "-H", "Content-Type: application/json",
        "-H", f"Authorization: Bearer {api_key}",
    ]
    for k, v in headers.items():
        curl_headers.extend(["-H", f"{k}: {v}"])

    result = subprocess.run(
        ["curl", "-s", url] + curl_headers + ["-d", json.dumps(body)],
        capture_output=True, text=True, timeout=60,
    )
    resp = json.loads(result.stdout)

    # Extract metrics
    usage = resp.get("usage", {})
    details = usage.get("prompt_tokens_details", {}) or {}
    cache_r = details.get("cached_tokens", 0) or 0
    cache_w = details.get("cache_write_tokens", 0) or 0
    if cache_w == 0 and cache_r == 0:
        cache_w = usage.get("cache_creation_input_tokens", 0) or 0
        cache_r = usage.get("cache_read_input_tokens", 0) or 0
    prompt = usage.get("prompt_tokens", 0) or 0

    # Extract assistant text
    choices = resp.get("choices", [])
    text = choices[0]["message"]["content"] if choices else ""

    return text, prompt, cache_r, cache_w


def send_anthropic_format(messages, system_prompt, api_key, headers,
                          base_url="https://openrouter.ai/api/v1",
                          tools=True):
    """Send via /v1/messages (Anthropic format)."""
    url = f"{base_url}/messages"

    # Build system as array of blocks with cache_control on last
    system_blocks = [{"type": "text", "text": system_prompt,
                      "cache_control": {"type": "ephemeral"}}]

    # Apply sliding breakpoints to messages (Shore-style)
    msgs = apply_sillytavern_sliding_to_anthropic(messages)

    body = {
        "model": "anthropic/claude-sonnet-4-6",
        "system": system_blocks,
        "messages": msgs,
        "max_tokens": 256,
    }
    if tools:
        body["tools"] = TOOLS_ANTHROPIC

    curl_headers = [
        "-H", "Content-Type: application/json",
        "-H", f"Authorization: Bearer {api_key}",
        "-H", "anthropic-version: 2023-06-01",
    ]
    for k, v in headers.items():
        curl_headers.extend(["-H", f"{k}: {v}"])

    result = subprocess.run(
        ["curl", "-s", url] + curl_headers + ["-d", json.dumps(body)],
        capture_output=True, text=True, timeout=60,
    )
    resp = json.loads(result.stdout)

    usage = resp.get("usage", {})
    cache_r = usage.get("cache_read_input_tokens", 0) or 0
    cache_w = usage.get("cache_creation_input_tokens", 0) or 0
    prompt = usage.get("input_tokens", 0) or 0

    # Extract assistant text
    content = resp.get("content", [])
    text = ""
    for block in content:
        if block.get("type") == "text":
            text += block.get("text", "")

    return text, prompt, cache_r, cache_w


def apply_sillytavern_sliding_to_anthropic(messages, caching_depth=2):
    """Apply SillyTavern-style sliding breakpoints to Anthropic-format messages.
    (System prompt is handled separately in Anthropic format.)"""
    msgs = copy.deepcopy(messages)

    passed_prefill = False
    depth = 0
    prev_role = ""
    for i in range(len(msgs) - 1, -1, -1):
        role = msgs[i]["role"]
        if not passed_prefill and role == "assistant":
            continue
        passed_prefill = True
        if role != prev_role:
            if depth == caching_depth or depth == caching_depth + 2:
                content = msgs[i]["content"]
                if isinstance(content, str):
                    msgs[i]["content"] = [{"type": "text", "text": content,
                                           "cache_control": {"type": "ephemeral"}}]
                elif isinstance(content, list):
                    content[-1]["cache_control"] = {"type": "ephemeral"}
            if depth == caching_depth + 2:
                break
            depth += 1
            prev_role = role

    return msgs


# ── Experiment runner ─────────────────────────────────────────────

def run_experiment(config):
    """Run a single experiment with the given config dict."""
    api_key = os.environ.get("OPENROUTER_SHORE_TEST", "")
    anthropic_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not api_key and config["format"] != "anthropic-direct":
        print(f"{R}OPENROUTER_SHORE_TEST not set{NC}", file=sys.stderr)
        return None
    if config["format"] == "anthropic-direct" and not anthropic_key:
        print(f"{Y}Skipping anthropic-direct (no ANTHROPIC_API_KEY){NC}")
        return None

    name = config["name"]
    nonce = base64.b64encode(os.urandom(24)).decode().replace(
        "/", "").replace("+", "").replace("=", "")[:32]
    system_prompt = make_system_prompt(nonce)
    delay = config.get("delay", 4)
    turns = config.get("turns", 10)
    warmup = config.get("warmup", 0)
    fmt = config["format"]
    bp_strategy = config.get("breakpoints", "sillytavern")
    use_tools = config.get("tools", True)
    provider_pin = config.get("provider_pin", None)

    headers = {}
    if config.get("sillytavern_headers", False):
        headers["HTTP-Referer"] = "https://sillytavern.app"
        headers["X-Title"] = "SillyTavern"

    print(f"\n{'='*60}")
    print(f"{C}[{name}]{NC} starting")
    print(f"  format={fmt} delay={delay}s bp={bp_strategy} "
          f"headers={'ST' if headers else 'none'} tools={use_tools} "
          f"warmup={warmup}")
    print(f"  nonce={nonce}")
    print(f"{'='*60}")

    # Pre-seed warmup messages if requested
    history = []
    for w in range(warmup):
        history.append({"role": "user", "content": f"Warmup message {w+1}."})
        history.append({"role": "assistant", "content": f"Acknowledged {w+1}."})

    results = []
    first_write = 0
    threshold = 0

    for turn in range(turns):
        a, b = random.randint(0, 99), random.randint(0, 99)
        user_text = f"Cache test turn {turn + 1}. What is {a} plus {b}?"

        history.append({"role": "user", "content": user_text})

        try:
            if fmt == "openai":
                all_msgs = [{"role": "system", "content": system_prompt}] + history
                if bp_strategy == "sillytavern":
                    all_msgs = apply_sillytavern_caching(all_msgs)
                elif bp_strategy == "system-only":
                    all_msgs = apply_system_only_caching(all_msgs)
                elif bp_strategy == "shore":
                    all_msgs = apply_shore_style_caching(
                        all_msgs,
                        depth_turns=config.get("depth_turns"),
                        pinned=config.get("pinned"))
                text, prompt, cache_r, cache_w = send_openai_format(
                    all_msgs, api_key, headers, tools=use_tools,
                    provider_pin=provider_pin)

            elif fmt == "anthropic":
                if bp_strategy == "system-only":
                    msgs = copy.deepcopy(history)  # no message breakpoints
                else:
                    msgs = apply_sillytavern_sliding_to_anthropic(history)
                text, prompt, cache_r, cache_w = send_anthropic_format(
                    msgs, system_prompt, api_key, headers, tools=use_tools)

            elif fmt == "anthropic-direct":
                if bp_strategy == "system-only":
                    msgs = copy.deepcopy(history)
                else:
                    msgs = apply_sillytavern_sliding_to_anthropic(history)
                text, prompt, cache_r, cache_w = send_anthropic_format(
                    msgs, system_prompt, anthropic_key, headers,
                    base_url="https://api.anthropic.com/v1", tools=use_tools)

            else:
                raise ValueError(f"Unknown format: {fmt}")

        except Exception as e:
            print(f"{R}[{name}]{NC} turn {turn}: error: {e}")
            results.append({"turn": turn, "error": str(e)})
            history.append({"role": "assistant", "content": "(error)"})
            if turn < turns - 1:
                time.sleep(delay)
            continue

        history.append({"role": "assistant", "content": text})

        is_rewrite = False
        if turn == 0:
            first_write = cache_w
            threshold = first_write // 2
            print(f"{C}[{name}]{NC} t{turn}: w={cache_w} r={cache_r} p={prompt} "
                  f"(cold, thresh={threshold})")
        else:
            is_rewrite = threshold > 0 and cache_w > threshold
            tag = f" {R}REWRITE{NC}" if is_rewrite else ""
            print(f"{C}[{name}]{NC} t{turn}: w={cache_w} r={cache_r} p={prompt}{tag}")

        results.append({
            "turn": turn, "prompt": prompt, "cache_r": cache_r,
            "cache_w": cache_w, "is_rewrite": is_rewrite,
        })

        if turn < turns - 1:
            time.sleep(delay)

    # Summary
    rewrites = sum(1 for r in results if r.get("is_rewrite"))
    non_cold = len([r for r in results if r["turn"] > 0 and "error" not in r])
    miss_rate = rewrites / non_cold if non_cold > 0 else 0

    color = G if rewrites == 0 else (Y if rewrites <= 1 else R)
    print(f"\n{color}[{name}] {rewrites}/{non_cold} rewrites "
          f"({miss_rate:.0%} miss rate){NC}")

    return {
        "name": name,
        "config": {k: v for k, v in config.items() if k != "name"},
        "nonce": nonce,
        "timestamp": datetime.now().isoformat(),
        "first_write": first_write,
        "threshold": threshold,
        "rewrites": rewrites,
        "non_cold_turns": non_cold,
        "miss_rate": miss_rate,
        "turns": results,
    }


# ── Experiment definitions ────────────────────────────────────────

EXPERIMENTS = [
    # Group A: Delay sweep (OpenAI format, ST headers, ST breakpoints)
    {"name": "A1-openai-st-4s",   "format": "openai", "sillytavern_headers": True,
     "breakpoints": "sillytavern", "delay": 4},
    {"name": "A2-openai-st-8s",   "format": "openai", "sillytavern_headers": True,
     "breakpoints": "sillytavern", "delay": 8},
    {"name": "A3-openai-st-15s",  "format": "openai", "sillytavern_headers": True,
     "breakpoints": "sillytavern", "delay": 15},

    # Group B: Header isolation (OpenAI format, 8s delay)
    {"name": "B1-openai-noheader-8s", "format": "openai", "sillytavern_headers": False,
     "breakpoints": "sillytavern", "delay": 8},

    # Group C: Format isolation (Anthropic format, ST headers, 8s delay)
    {"name": "C1-anthropic-st-8s", "format": "anthropic", "sillytavern_headers": True,
     "breakpoints": "sillytavern", "delay": 8},
    {"name": "C2-anthropic-noheader-8s", "format": "anthropic", "sillytavern_headers": False,
     "breakpoints": "sillytavern", "delay": 8},

    # Group D: Breakpoint strategy (OpenAI format, ST headers, 8s delay)
    {"name": "D1-openai-sysonly-8s", "format": "openai", "sillytavern_headers": True,
     "breakpoints": "system-only", "delay": 8},

    # Group E: Warmup (OpenAI, ST headers, 8s, pre-seed 3 turns)
    {"name": "E1-openai-warmup3-8s", "format": "openai", "sillytavern_headers": True,
     "breakpoints": "sillytavern", "delay": 8, "warmup": 3},

    # Group F: Direct Anthropic (no OpenRouter, 8s delay)
    {"name": "F1-direct-anthropic-8s", "format": "anthropic-direct",
     "sillytavern_headers": False, "breakpoints": "sillytavern", "delay": 8},

    # Group G: Provider pinning (OpenAI format, 8s delay)
    {"name": "G1-openai-pin-anthropic-8s", "format": "openai",
     "sillytavern_headers": True, "breakpoints": "sillytavern", "delay": 8,
     "provider_pin": "Anthropic"},
    {"name": "G2-openai-pin-anthropic-4s", "format": "openai",
     "sillytavern_headers": True, "breakpoints": "sillytavern", "delay": 4,
     "provider_pin": "Anthropic"},
    {"name": "G3-openai-sysonly-pin-4s", "format": "openai",
     "sillytavern_headers": True, "breakpoints": "system-only", "delay": 4,
     "provider_pin": "Anthropic"},

    # Group H: Breakpoint combos (OpenAI, 3s delay, 6 turns, no pinning)
    # sliding × pinned matrix
    {"name": "H1-d12-pin-1", "format": "openai", "breakpoints": "shore",
     "depth_turns": [1, 2], "pinned": -1, "delay": 3, "turns": 6},
    {"name": "H2-d12-pin0", "format": "openai", "breakpoints": "shore",
     "depth_turns": [1, 2], "pinned": 0, "delay": 3, "turns": 6},
    {"name": "H3-d12-nopin", "format": "openai", "breakpoints": "shore",
     "depth_turns": [1, 2], "pinned": None, "delay": 3, "turns": 6},
    {"name": "H4-d01-pin-1", "format": "openai", "breakpoints": "shore",
     "depth_turns": [0, 1], "pinned": -1, "delay": 3, "turns": 6},
    {"name": "H5-d01-pin0", "format": "openai", "breakpoints": "shore",
     "depth_turns": [0, 1], "pinned": 0, "delay": 3, "turns": 6},
    {"name": "H6-d01-nopin", "format": "openai", "breakpoints": "shore",
     "depth_turns": [0, 1], "pinned": None, "delay": 3, "turns": 6},
    {"name": "H7-nosl-pin-1", "format": "openai", "breakpoints": "shore",
     "depth_turns": None, "pinned": -1, "delay": 3, "turns": 6},
    {"name": "H8-nosl-pin0", "format": "openai", "breakpoints": "shore",
     "depth_turns": None, "pinned": 0, "delay": 3, "turns": 6},

    # H9: provider pin control — retest G with fresh credits
    {"name": "H9-pin-control", "format": "openai", "breakpoints": "sillytavern",
     "delay": 3, "turns": 6, "provider_pin": "Anthropic",
     "sillytavern_headers": True},
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--only", help="Run only experiments matching this prefix")
    parser.add_argument("--results", default="scripts/cache-tests/experiment-results.jsonl",
                        help="JSONL file for results")
    args = parser.parse_args()

    experiments = EXPERIMENTS
    if args.only:
        experiments = [e for e in experiments if e["name"].startswith(args.only)]

    if not experiments:
        print(f"{R}No experiments match '{args.only}'{NC}")
        sys.exit(1)

    print(f"Running {len(experiments)} experiments, results → {args.results}")

    all_results = []
    for exp in experiments:
        result = run_experiment(exp)
        if result:
            all_results.append(result)
            with open(args.results, "a") as f:
                f.write(json.dumps(result) + "\n")

    # Final summary table
    print(f"\n{'='*70}")
    print(f"{'Experiment':<30} {'Rewrites':>10} {'Miss Rate':>10} {'Result':>8}")
    print(f"{'-'*70}")
    for r in all_results:
        color = G if r["rewrites"] == 0 else (Y if r["rewrites"] <= 1 else R)
        status = "PASS" if r["rewrites"] == 0 else "FAIL"
        print(f"{r['name']:<30} {r['rewrites']:>5}/{r['non_cold_turns']:<4} "
              f"{r['miss_rate']:>9.0%} {color}{status:>8}{NC}")
    print(f"{'='*70}")


if __name__ == "__main__":
    main()
