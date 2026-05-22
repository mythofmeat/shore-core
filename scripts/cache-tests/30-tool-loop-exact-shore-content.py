#!/usr/bin/env python3
"""
Tool-loop cache probe using Shore's EXACT system prompt text and EXACT
tools array, lifted byte-for-byte from a body dump captured by Shore's
SHORE_OPENAI_BODY_DUMP=1 diagnostic. The daemon logs the dump path on
first write ("SHORE_OPENAI_BODY_DUMP active"); it lives in a per-process
random tempdir under $TMPDIR (prefix shore-openai-body-dumps-).

This is the control experiment that should have been run from the start.
It isolates wire-path bugs from content-driven model behavior. If the
probe — which we know works on simpler content — fails on Shore's content,
the failure is the model's response to that content (upstream of Shore).
If the probe SUCCEEDS on Shore's content, then Shore's daemon wire path
is doing something different from what this probe does, and the cache
rewrite is on us.

Reads:
  /tmp/shore-system.json — Shore's system prompt text (JSON string)
  /tmp/shore-tools.json  — Shore's tools array
"""

import json
import os
import subprocess
import sys
import time
import base64
import pathlib

MODEL = "anthropic/claude-sonnet-4-6"
URL = "https://openrouter.ai/api/v1/chat/completions"
API_KEY = (os.environ.get("OPENROUTER_SHORE_TEST")
           or os.environ.get("OPENROUTER_API_KEY", ""))
DELAY = 1

R = "\033[0;31m"
G = "\033[0;32m"
C = "\033[0;36m"
Y = "\033[0;33m"
NC = "\033[0m"
NAME = "exact-shore-content"

# Load Shore's actual content.
SYSTEM_TEXT = json.loads(pathlib.Path("/tmp/shore-system.json").read_text())
TOOLS = json.loads(pathlib.Path("/tmp/shore-tools.json").read_text())


def apply_cache_markers(messages):
    """Match what Shore's openai.rs does: mark the most recent user message
    only, plus the system block. Same strategy as current Shore code."""
    import copy
    msgs = copy.deepcopy(messages)
    cc = {"type": "ephemeral"}

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


def send(messages, streaming):
    # Mimic Shore: emit all user messages with array content (matching
    # what openai.rs::translate_messages produces from Anthropic-format
    # Message structs).
    messages = [dict(m) for m in messages]
    for m in messages:
        if m.get("role") == "user" and isinstance(m.get("content"), str):
            m["content"] = [{"type": "text", "text": m["content"]}]
    annotated_msgs = apply_cache_markers(messages)

    system_msg = {
        "role": "system",
        "content": [{
            "type": "text",
            "text": SYSTEM_TEXT,
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
        "temperature": 1.0,
        "usage": {"include": True},
    }
    if streaming:
        body["stream"] = True
        body["stream_options"] = {"include_usage": True}

    import os as _os
    if _os.environ.get("PROBE_BODY_DUMP"):
        _dir = pathlib.Path("/tmp/probe-body-dumps")
        _dir.mkdir(exist_ok=True)
        _stamp = int(time.time() * 1_000_000)
        (_dir / f"call_{_stamp}_{int(streaming)}.json").write_text(json.dumps(body, indent=2))
    result = subprocess.run(
        ["curl", "-s", URL,
         "-H", "Content-Type: application/json",
         "-H", f"Authorization: Bearer {API_KEY}",
         "-d", json.dumps(body)],
        capture_output=True, text=True, timeout=120,
    )

    if streaming:
        # Parse SSE: find the final usage chunk, plus reconstruct the message.
        final_msg = {"content": "", "tool_calls": [], "reasoning_details": []}
        usage = None
        for line in result.stdout.splitlines():
            if not line.startswith("data: "):
                continue
            data = line[6:].strip()
            if data == "[DONE]":
                break
            try:
                chunk = json.loads(data)
            except json.JSONDecodeError:
                continue
            choices = chunk.get("choices", [])
            if choices:
                delta = choices[0].get("delta", {}) or {}
                if "content" in delta and delta["content"]:
                    final_msg["content"] += delta["content"]
                if delta.get("tool_calls"):
                    for tc_delta in delta["tool_calls"]:
                        idx = tc_delta.get("index", 0)
                        while len(final_msg["tool_calls"]) <= idx:
                            final_msg["tool_calls"].append({
                                "id": "", "type": "function",
                                "function": {"name": "", "arguments": ""}
                            })
                        tc = final_msg["tool_calls"][idx]
                        if tc_delta.get("id"):
                            tc["id"] = tc_delta["id"]
                        fn = tc_delta.get("function") or {}
                        if fn.get("name"):
                            tc["function"]["name"] = fn["name"]
                        if "arguments" in fn:
                            tc["function"]["arguments"] += fn.get("arguments") or ""
                if delta.get("reasoning_details"):
                    final_msg["reasoning_details"].extend(delta["reasoning_details"])
            if chunk.get("usage"):
                usage = chunk["usage"]
        if usage is None:
            print(f"{R}[{NAME}]{NC} no usage in SSE response", file=sys.stderr)
            print(result.stdout[:500])
            sys.exit(1)
        pt_details = usage.get("prompt_tokens_details") or {}
        cached_tokens = pt_details.get("cached_tokens", 0) or 0
        return None, final_msg, {
            "prompt": usage.get("prompt_tokens", 0) or 0,
            "completion": usage.get("completion_tokens", 0) or 0,
            "cache_r": cached_tokens,
            "cache_w": pt_details.get("cache_write_tokens", 0) or 0,
        }

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
    return resp, msg, {
        "prompt": usage.get("prompt_tokens", 0) or 0,
        "completion": usage.get("completion_tokens", 0) or 0,
        "cache_r": cached_tokens,
        "cache_w": pt_details.get("cache_write_tokens", 0) or 0,
    }


def log(step, label, u, threshold):
    rewrite = threshold > 0 and u["cache_w"] > threshold
    tag = f" {R}*** REWRITE ***{NC}" if rewrite else ""
    print(f"{C}[{NAME}]{NC} {step}: {label}")
    print(f"  prompt={u['prompt']} cache_r={u['cache_r']} cache_w={u['cache_w']}{tag}")
    return rewrite


def build_assistant_msg(msg):
    out = {"role": "assistant"}
    # Force-include content (None → null), matching Shore openai.rs.
    c = msg.get("content")
    out["content"] = c if (c is not None and c != "") else None
    if msg.get("tool_calls"):
        out["tool_calls"] = msg["tool_calls"]
    if msg.get("reasoning_details"):
        out["reasoning_details"] = msg["reasoning_details"]
    return out


def run_flow(streaming):
    mode = "STREAMING" if streaming else "NON-STREAMING"
    print(f"{C}[{NAME}]{NC} ====== {mode} ======")
    messages = []
    threshold = 0
    rewrites = 0
    step = 0

    def do_turn(user_msg, label):
        nonlocal step, threshold, rewrites
        messages.append({"role": "user", "content": user_msg})
        _, msg, u = send(messages, streaming)
        if step == 0:
            threshold = u["prompt"] // 2
            log(f"s{step}", f"{label} (cold)", u, 0)
        else:
            # Threshold: rewrite if cache_w > 30% of prompt (heuristic).
            if u["cache_w"] > u["prompt"] * 0.3:
                rewrites += 1
                log(f"s{step}", label + f" CACHE-REWRITE (w={u['cache_w']}/p={u['prompt']})",
                    u, threshold)
            else:
                log(f"s{step}", label, u, 0)
        rd = msg.get("reasoning_details") or []
        tcs = msg.get("tool_calls") or []
        text = msg.get("content") or ""
        print(f"  → reasoning_dets={len(rd)} tools={len(tcs)} "
              f"text={repr(text[:50]) if text else '(none)'}")
        messages.append(build_assistant_msg(msg))
        step += 1
        time.sleep(DELAY)

        loop_count = 0
        while tcs:
            loop_count += 1
            if loop_count > 3:
                break
            for tc in tcs:
                fname = tc.get("function", {}).get("name", "?")
                args = tc.get("function", {}).get("arguments", "")
                print(f"  🔧 {fname}({args[:40]})")
                messages.append({
                    "role": "tool",
                    "tool_call_id": tc.get("id", ""),
                    "content": "Friday, May 22nd, 2026 at 12:14 AM",
                })

            _, msg, u = send(messages, streaming)
            if u["cache_w"] > u["prompt"] * 0.3:
                rewrites += 1
                log(f"s{step}", f"tool-loop-{loop_count} CACHE-REWRITE (w={u['cache_w']}/p={u['prompt']})", u, threshold)
            else:
                log(f"s{step}", f"tool-loop-{loop_count}", u, 0)

            rd = msg.get("reasoning_details") or []
            tcs = msg.get("tool_calls") or []
            text = msg.get("content") or ""
            print(f"  → reasoning_dets={len(rd)} tools={len(tcs)} "
                  f"text={repr(text[:50]) if text else '(none)'}")
            messages.append(build_assistant_msg(msg))
            step += 1
            time.sleep(DELAY)

    do_turn("Warm-up one. Reply with only WARM1.", "warm-up-1")
    do_turn("Warm-up two. Reply with only WARM2.", "warm-up-2")
    do_turn(
        "Use the check_time tool exactly once before answering. "
        "After the tool result, reply with only TIME_OK.",
        "tool-trigger")

    print(f"\n{C}[{NAME}]{NC} {mode}: {rewrites} rewrite(s) in {step-1} subsequent turns")
    return rewrites


def main():
    if not API_KEY:
        print(f"{R}OPENROUTER_SHORE_TEST or OPENROUTER_API_KEY not set{NC}", file=sys.stderr)
        sys.exit(1)
    print(f"{C}[{NAME}]{NC} system size: {len(SYSTEM_TEXT)} chars")
    print(f"{C}[{NAME}]{NC} tools: {[t['function']['name'] for t in TOOLS]}")
    print()

    # Run both streaming and non-streaming on identical content.
    # Shore daemon uses streaming; non-streaming gives a clean baseline.
    n_rewrites_nostream = run_flow(streaming=False)
    print()
    n_rewrites_stream = run_flow(streaming=True)

    print()
    print(f"{C}[{NAME}]{NC} FINAL: non-streaming rewrites={n_rewrites_nostream}, "
          f"streaming rewrites={n_rewrites_stream}")
    sys.exit(0 if (n_rewrites_nostream + n_rewrites_stream) == 0 else 1)


if __name__ == "__main__":
    main()
