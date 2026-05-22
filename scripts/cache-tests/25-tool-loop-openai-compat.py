#!/usr/bin/env python3
"""
Cache stability test on OpenRouter via /chat/completions (OpenAI-compatible).

Same flow as 19-tool-loop.py, different wire path:
- Endpoint: /v1/chat/completions instead of /v1/messages
- Reasoning: OpenRouter "reasoning" field with effort=high (maps to adaptive)
- Cache markers: cache_control extensions on content blocks
- Replay: preserve reasoning_details on assistant messages across turns

The question this probe answers: does OpenRouter's /chat/completions path
preserve cache continuity through tool-result continuations when adaptive
thinking rolls zero-think on a trivial tool call? That is the exact bare-
tool_use shape that breaks Shore's current /messages path.
"""

import json
import os
import subprocess
import sys
import time
import base64

MODEL = "anthropic/claude-sonnet-4-6"
URL = "https://openrouter.ai/api/v1/chat/completions"
API_KEY = os.environ.get("OPENROUTER_SHORE_TEST", "")
DELAY = 3

R = "\033[0;31m"
G = "\033[0;32m"
C = "\033[0;36m"
Y = "\033[0;33m"
NC = "\033[0m"
NAME = "tool-loop-cc"

NONCE = base64.b64encode(os.urandom(24)).decode().replace(
    "/", "").replace("+", "").replace("=", "")[:32]

# Same padding as 19-tool-loop.py — keep system prefix byte-identical in
# semantics so we can compare cache numbers across the two endpoints.
SYSTEM_PROMPT = f"""\
You are a helpful assistant with tools. Always use tools when asked.

NONCE: {NONCE}

--- BEGIN PADDING ---

This padding exists to ensure the system prompt exceeds the provider's minimum
for prompt caching. The content below is stable reference material.

Section 1: Prompt caching reduces redundant computation when the same token
prefix appears across multiple API calls. The API compares incoming tokens
from the beginning and serves matching prefixes from cache. Cache entries
have a configurable TTL. Cache writes cost 25% more than base input pricing.
Cache reads cost 90% less. For a 1-hour TTL, up to 19 keepalive pings are
economically justified.

Section 2: Key metrics are cache_read_tokens and cache_creation_tokens in the
usage object. A cache hit shows cache_read_tokens > 0 and cache_creation_tokens
= 0. A cache miss shows cache_creation_tokens > 0. The prefix hash helps
identify whether content changed between calls.

Section 3: Thinking mode changes invalidate the prefix. Content format
normalization between string and array formats causes cache invalidation.
Cache marker movement does not invalidate the prefix.

Section 4: Cache TTL is 1 hour. Keepalive interval is 59 minutes. Minimum
cacheable prefix is 1024 tokens for some models and 2048 for others. The
cache_control annotation uses type ephemeral with optional ttl parameter.
Multiple breakpoints can exist per request up to a maximum of 4.

Section 5: The Anthropic Messages API uses byte-pair encoding tokenization.
Common English words are single tokens. Rare words and technical terms may
need multiple tokens. On average one token equals approximately 3.5-4
characters of English text. Cache write premium is 25% over base input
pricing. Cache read discount is 90% off base input pricing. Break-even
depends on reuse count within the TTL window.

Section 6: The usage object contains input_tokens, output_tokens,
cache_creation_input_tokens, and cache_read_input_tokens. The streaming
interface uses SSE with event types message_start, content_block_start,
content_block_delta, content_block_stop, message_delta, and message_stop.

Section 7: The model field specifies which Claude model to use. The max_tokens
field sets the upper bound on output tokens. The messages field contains
conversation history. Content blocks can be text, image, tool_use, or
tool_result. The system parameter accepts a string or array of content blocks.

Section 8: The Messages API supports multi-turn conversations where each
message has a role and content. Tool use follows a specific flow: the model
returns a tool_use content block with a unique ID, then the client sends a
tool_result with matching ID. Multiple tool uses can occur in a single turn.
The model may chain tool calls by requesting additional tools after receiving
results. This chaining behavior is key to complex multi-step workflows.

Section 9: Server-Sent Events deliver incremental response data. The
message_start event contains the message metadata and usage statistics.
Content blocks are delimited by content_block_start and content_block_stop
events. Text arrives as content_block_delta events with text_delta payloads.

Section 10: The metadata field accepts arbitrary key-value pairs for request
tracking. The stop_sequences field specifies custom strings that halt
generation. System prompts support cache_control annotations for prompt caching.
The anthropic-beta header enables experimental features like extended thinking.

Section 11: Response validation and error handling require attention to several
fields. The stop_reason field indicates why generation stopped: end_turn for
normal completion, max_tokens when the output limit is reached, stop_sequence
when a custom stop string is matched, and tool_use when the model wants to
call a tool. Each content block has a type field that determines its structure.
Text blocks contain a text field. Tool use blocks contain name, id, and input
fields. The id field is critical for matching tool results to tool calls.

Section 12: Multi-turn conversation management involves careful message
ordering. User messages alternate with assistant messages. Tool results are
sent as user messages with content arrays containing tool_result blocks. Each
tool_result must include the tool_use_id from the corresponding tool_use block.
The model maintains context across turns through the message history. System
prompts provide persistent instructions that apply to all turns.

Section 13: Advanced prompting techniques include few-shot examples embedded
in the system prompt, chain-of-thought reasoning through explicit thinking
instructions, and structured output formatting using XML tags or JSON schemas.
Temperature controls output randomness with values from 0 to 1. Top-p nucleus
sampling provides an alternative randomness control. Top-k limits the
vocabulary considered at each generation step.

Section 14: Token counting and cost management require understanding the
relationship between text length and token count. The tokenizer splits text
into subword units. Common words are single tokens while rare words may be
multiple tokens. Whitespace and punctuation affect tokenization. JSON
structure adds overhead tokens for braces, commas, colons, and quotes.
Tool definitions contribute significantly to input token counts since each
tool name, description, and parameter schema is tokenized.

Section 15: Rate limiting and retry strategies are essential for production
systems. The API returns 429 status codes when rate limits are exceeded. The
retry-after header indicates how long to wait before retrying. Exponential
backoff with jitter prevents thundering herd problems. Connection pooling
reduces overhead for repeated requests. Streaming responses begin delivering
data sooner than non-streaming requests, reducing perceived latency.

Section 16: Security considerations include input validation to prevent
prompt injection, output filtering for sensitive content, API key rotation
and secure storage, request logging for audit trails, and access control
for multi-tenant systems. The API supports custom headers for request
identification and tracking through proxy servers and load balancers.

Section 17: Connection management for streaming responses requires careful
handling of server-sent event streams. The client must maintain an open HTTP
connection for the duration of the response. Network interruptions can cause
partial responses that need to be handled gracefully. Implementing automatic
reconnection with resume tokens allows recovery from transient failures without
losing generated content or wasting compute resources on regeneration.

Section 18: Batch processing capabilities allow multiple independent requests
to be submitted together for efficient processing. Each request in a batch
operates independently with its own system prompt, messages, and parameters.
Batch results are returned asynchronously and can be polled or received via
webhook notification. This is ideal for offline processing tasks like document
summarization, data extraction, and content classification at scale.

Section 19: Model selection criteria depend on the specific use case requirements
including response quality, latency, cost, and context window size. Larger models
like Opus provide higher quality reasoning and more nuanced responses but at
higher cost and latency. Smaller models like Haiku offer faster responses at
lower cost, suitable for classification, extraction, and simple generation tasks.
Sonnet provides a balance between quality and efficiency for most applications.

Section 20: Prompt engineering best practices include being specific about desired
output format, providing relevant context without unnecessary information,
using clear and unambiguous language, structuring complex instructions with
numbered steps or bullet points, and including examples of desired behavior.
System prompts should establish the assistant role, constraints, and output
expectations. User messages should contain the specific request and any
necessary input data for processing.

Section 21: Content moderation and safety features are built into all Claude
models. The API automatically filters harmful content and refuses dangerous
requests. Custom safety settings can be configured through system prompts
that specify acceptable topics and response boundaries. Output monitoring
should be implemented for production systems to catch edge cases and maintain
quality standards across all user interactions and conversation contexts.

Section 22: Integration patterns for production systems include synchronous
request-response for interactive applications, asynchronous processing with
message queues for batch workloads, streaming for real-time display of
generated content, and webhook-based notification for long-running tasks.
Each pattern has different requirements for error handling, retry logic,
timeout configuration, and resource management that must be carefully
considered during system design and implementation phases.

--- END PADDING ---

IMPORTANT: When asked to do multiple things, use tools one at a time. After
getting a tool result, evaluate if you need another tool before responding."""

# OpenAI-style tool schemas (function wrapper around an input schema).
def fn_tool(name, desc, properties, required):
    return {
        "type": "function",
        "function": {
            "name": name,
            "description": desc,
            "parameters": {
                "type": "object",
                "properties": properties,
                "required": required,
            },
        },
    }


TOOLS = [
    fn_tool("check_time", "Check the current date and time.", {}, []),
    fn_tool("web_search", "Search the web for current information.",
            {"query": {"type": "string"}}, ["query"]),
    fn_tool("roll_dice", "Roll dice using standard notation like 2d6+3.",
            {"notation": {"type": "string"}}, ["notation"]),
    fn_tool("memory_search", "Search markdown memory files.",
            {"query": {"type": "string"}}, ["query"]),
    fn_tool("scratchpad_write", "Write content to a scratchpad file.",
            {"path": {"type": "string"}, "content": {"type": "string"}},
            ["path", "content"]),
    fn_tool("scratchpad_read", "Read content from a scratchpad file.",
            {"path": {"type": "string"}}, ["path"]),
    fn_tool("scratchpad_list", "List files in the scratchpad directory.",
            {"path": {"type": "string"}}, []),
    fn_tool("fetch_url", "Fetch content from a URL.",
            {"url": {"type": "string"}}, ["url"]),
    fn_tool("send_image", "Send an image to the user.",
            {"path": {"type": "string"}, "caption": {"type": "string"}},
            ["path"]),
    fn_tool("generate_image", "Generate an image from a text prompt.",
            {"prompt": {"type": "string"}, "size": {"type": "string"}},
            ["prompt"]),
    fn_tool("activity_heatmap", "Generate activity heatmap visualization.",
            {"days": {"type": "integer"}}, []),
]

TOOL_RESPONSES = {
    "check_time": "Current time: 2026-04-09T19:45:00Z (Wednesday evening UTC)",
    "web_search": "Search results: Tokyo weather 18°C partly cloudy.",
    "roll_dice": "Result: 14 (rolled 3d6: 5, 4, 5)",
}


def apply_cache_markers(messages):
    """Mark the last user/tool message with OpenRouter cache_control.

    OpenRouter's /chat/completions accepts cache_control as an extension on
    content blocks when content is an array. We mirror Anthropic's
    sliding-breakpoint strategy: mark the most recent user-side boundary,
    which advances onto each completed tool result.
    """
    import copy
    msgs = copy.deepcopy(messages)
    cc = {"type": "ephemeral"}

    # Find last two user-side messages (user or tool role).
    user_side = [i for i, m in enumerate(msgs)
                 if m.get("role") in ("user", "tool")]
    targets = user_side[-2:] if len(user_side) >= 2 else user_side[-1:]

    for idx in targets:
        msg = msgs[idx]
        content = msg.get("content")
        if isinstance(content, str):
            msg["content"] = [{"type": "text", "text": content,
                               "cache_control": cc}]
        elif isinstance(content, list):
            for block in reversed(content):
                if isinstance(block, dict):
                    block["cache_control"] = cc
                    break
    return msgs


def send(messages, system_message):
    annotated_msgs = apply_cache_markers(messages)

    # System block as a content array so we can pin cache_control on it.
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
    # OpenRouter returns cache stats under prompt_tokens_details for Anthropic
    # routes; some routes also expose cache_creation_input_tokens directly.
    pt_details = usage.get("prompt_tokens_details") or {}
    cached_tokens = pt_details.get("cached_tokens", 0) or 0
    # OpenRouter sometimes uses cache_discount or distinct cache fields too.
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
    """Echo the assistant message verbatim, preserving reasoning_details."""
    out = {"role": "assistant"}
    if msg.get("content") is not None:
        out["content"] = msg["content"]
    if msg.get("tool_calls"):
        out["tool_calls"] = msg["tool_calls"]
    if msg.get("reasoning_details") is not None:
        out["reasoning_details"] = msg["reasoning_details"]
    if msg.get("reasoning") is not None:
        out["reasoning"] = msg["reasoning"]
    return out


def main():
    if not API_KEY:
        print(f"{R}OPENROUTER_SHORE_TEST not set{NC}", file=sys.stderr)
        sys.exit(1)

    print(f"{C}[{NAME}]{NC} nonce: {NONCE}")
    print(f"{C}[{NAME}]{NC} endpoint=/chat/completions reasoning.effort=high pin=Anthropic")
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
            threshold = u["cache_w"] // 2
            log(f"s{step}", f"{label} (cold)", u, 0)
        else:
            if log(f"s{step}", label, u, threshold):
                rewrites += 1

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
            for tc in tool_calls:
                name = tc.get("function", {}).get("name", "?")
                args = tc.get("function", {}).get("arguments", "")
                print(f"  🔧 {name}({args[:60]})")

            # Build tool result messages.
            for tc in tool_calls:
                name = tc.get("function", {}).get("name", "?")
                fake = TOOL_RESPONSES.get(name, f"Tool {name} executed.")
                messages.append({
                    "role": "tool",
                    "tool_call_id": tc.get("id", ""),
                    "content": fake,
                })

            resp, msg, u = send(messages, SYSTEM_PROMPT)
            if log(f"s{step}", f"tool-loop-{loop_count}", u, threshold):
                rewrites += 1

            tool_calls = msg.get("tool_calls") or []
            text = msg.get("content") or ""
            reasoning_dets = msg.get("reasoning_details") or []
            print(f"  → reasoning_dets={len(reasoning_dets)} "
                  f"tools={len(tool_calls)} "
                  f"text={repr(text[:60]) if text else '(none)'}")

            messages.append(build_assistant_msg(msg))
            step += 1
            time.sleep(DELAY)

            if loop_count > 5:
                print(f"{Y}  (stopping tool loop after 5 iterations){NC}")
                break

    # ── Run the test ──────────────────────────────────────────────

    print(f"{C}[{NAME}]{NC} === WARM-UP ===")
    do_turn("What is 7 + 3?", "warm-up-1")

    print(f"\n{C}[{NAME}]{NC} === THINKING TURNS ===")
    do_turn(
        "What is the sum of all prime numbers between 50 and 100? "
        "Work through it carefully.",
        "think-hard-1")
    do_turn(
        "If I have a 3x3 matrix [[1,2,3],[4,5,6],[7,8,9]], "
        "what is its determinant? Show your reasoning.",
        "think-hard-2")
    do_turn(
        "A train leaves station A at 60mph. Another leaves station B "
        "(300 miles away) at 80mph heading toward A. They leave at the "
        "same time. When do they meet?",
        "think-hard-3")

    print(f"\n{C}[{NAME}]{NC} === TOOL USE LOOP ===")
    do_turn(
        "Okay, now I need you to do a few things with tools: "
        "First, check what time it is. "
        "Then, search the web for the weather in Tokyo. "
        "Then, roll 3d6 for me. "
        "Report all the results together at the end.",
        "multi-tool-request")

    print(f"\n{C}[{NAME}]{NC} === POST-TOOL FOLLOW-UP ===")
    do_turn("Thanks! Now what is 5 + 5?", "follow-up-1")
    do_turn("And 100 / 4?", "follow-up-2")

    total = step - 1
    color = G if rewrites == 0 else R
    print(f"\n{color}[{NAME}] {rewrites}/{total} rewrites{NC}")
    sys.exit(1 if rewrites > 0 else 0)


if __name__ == "__main__":
    main()
