#!/usr/bin/env python3
"""
Inline-system positioning probe.

The risk: when Shore routes Anthropic models through OpenRouter's
/chat/completions endpoint, any mid-history `role:"system"` message might
get collapsed into the top-level system prompt by the proxy. That would
(a) bust the cache by mutating the cached prefix on every turn and
(b) silently re-order instructions so they're seen before chat history
instead of at their actual conversation position.

This probe verifies that Shore's `<system_instruction>` user-wrap
strategy survives the wire path correctly. It runs the same fruit-order
conversation two ways:

  RAW: inject `role:"system"` blocks mid-history (no wrapping)
  WRAP: convert those blocks to `role:"user"` with
        `<system_instruction>...</system_instruction>` tags (Shore-style)

The conversation:
  user: I'm going to list 5 fruits.
  user: apple
  user: banana
  system: grape
  user: peach
  system: orange
  user: list the fruits in the exact order you saw them, one per line

Correct response order: apple, banana, grape, peach, orange.
If the model says "grape, orange, apple, banana, peach" it means the
system blocks were re-positioned ahead of the chat history.

Exits non-zero if the WRAP variant does NOT produce the correct order.
The RAW variant is informational — it tells us what OpenRouter does
with un-wrapped mid-history system blocks, but we don't fail on it.
"""

import json
import os
import re
import subprocess
import sys

MODEL = "anthropic/claude-sonnet-4-6"
URL = "https://openrouter.ai/api/v1/chat/completions"
API_KEY = (os.environ.get("OPENROUTER_SHORE_TEST")
           or os.environ.get("OPENROUTER_API_KEY", ""))

R = "\033[0;31m"
G = "\033[0;32m"
C = "\033[0;36m"
Y = "\033[0;33m"
NC = "\033[0m"
NAME = "fruit-order"

SYSTEM_PROMPT = (
    "You are a helpful assistant. Follow user instructions exactly. "
    "When asked to list items in order, output only the items "
    "themselves — no commentary, no numbering, one per line."
)

# Logical conversation shape. Role labels are the high-level ones;
# the two variants below render them into wire-level shapes differently.
LOGICAL_CONVERSATION = [
    ("user", "I'm going to list 5 fruits."),
    ("user", "apple"),
    ("user", "banana"),
    ("system", "grape"),
    ("user", "peach"),
    ("system", "orange"),
    ("user",
     "List the fruits in the exact order you saw them, one fruit per "
     "line, no commentary, no numbering."),
]

EXPECTED_ORDER = ["apple", "banana", "grape", "peach", "orange"]


def render_raw(logical):
    """Wire shape with raw `role:system` blocks mid-history."""
    return [{"role": role, "content": content} for role, content in logical]


def render_wrap(logical):
    """Wire shape with Shore-style `<system_instruction>` user wrap.

    Matches stream_helpers::wrap_inline_system_instruction format.
    """
    out = []
    for role, content in logical:
        if role == "system":
            wrapped = f"<system_instruction>{content}</system_instruction>"
            # If previous message is a user message, merge to avoid two
            # consecutive user turns (some providers reject that shape).
            # Mirrors convert_inline_system_messages in anthropic.rs.
            if out and out[-1]["role"] == "user":
                prev = out[-1]
                if isinstance(prev["content"], str):
                    prev["content"] = prev["content"] + "\n\n" + wrapped
                else:
                    prev["content"].append({"type": "text", "text": wrapped})
            else:
                out.append({"role": "user", "content": wrapped})
        else:
            out.append({"role": role, "content": content})
    return out


def send(messages, label):
    body = {
        "model": MODEL,
        "messages": [{"role": "system", "content": SYSTEM_PROMPT}] + messages,
        "max_tokens": 512,
        "provider": {"order": ["Anthropic"], "allow_fallbacks": False},
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
        print(f"{R}[{NAME}/{label}]{NC} non-JSON response: "
              f"{result.stdout[:500]}")
        return None

    if "error" in resp:
        print(f"{R}[{NAME}/{label}]{NC} API error: "
              f"{json.dumps(resp['error'], indent=2)}")
        return None

    choice = resp.get("choices", [{}])[0]
    msg = choice.get("message", {}) or {}
    return msg.get("content") or ""


def extract_order(text):
    """Return the list of EXPECTED fruits in the order they first appear."""
    if not text:
        return []
    lower = text.lower()
    positions = []
    for fruit in EXPECTED_ORDER:
        # Match whole-word fruit name.
        match = re.search(rf"\b{re.escape(fruit)}\b", lower)
        if match:
            positions.append((match.start(), fruit))
    positions.sort()
    return [fruit for _, fruit in positions]


def run_variant(label, render_fn):
    messages = render_fn(LOGICAL_CONVERSATION)
    print(f"{C}[{NAME}/{label}]{NC} sending {len(messages)} wire messages")
    for i, m in enumerate(messages):
        content_preview = (m["content"][:60] if isinstance(m["content"], str)
                           else "<blocks>")
        print(f"  [{i}] {m['role']}: {content_preview!r}")

    reply = send(messages, label)
    if reply is None:
        return None

    print(f"{C}[{NAME}/{label}]{NC} reply:\n  {reply.replace(chr(10), chr(10) + '  ')}")
    order = extract_order(reply)
    print(f"{C}[{NAME}/{label}]{NC} extracted order: {order}")
    return order


def main():
    if not API_KEY:
        print(f"{R}OPENROUTER_SHORE_TEST or OPENROUTER_API_KEY not set{NC}",
              file=sys.stderr)
        sys.exit(1)

    print(f"{C}[{NAME}]{NC} target: {MODEL} via {URL}")
    print(f"{C}[{NAME}]{NC} expected order: {EXPECTED_ORDER}")
    print()

    # Variant 1: WRAP — the strategy we want Shore to use.
    wrap_order = run_variant("WRAP", render_wrap)
    print()

    # Variant 2: RAW — informational, shows what OpenRouter does with
    # un-wrapped role:system mid-history.
    raw_order = run_variant("RAW", render_raw)
    print()

    # ── Verdict ───────────────────────────────────────────────────
    print(f"{C}[{NAME}]{NC} === summary ===")
    print(f"  WRAP order: {wrap_order}")
    print(f"  RAW  order: {raw_order}")
    print(f"  EXPECTED:   {EXPECTED_ORDER}")

    wrap_ok = wrap_order == EXPECTED_ORDER
    raw_ok = raw_order == EXPECTED_ORDER

    if wrap_ok:
        print(f"{G}[{NAME}] WRAP correct{NC}")
    else:
        print(f"{R}[{NAME}] WRAP WRONG — {wrap_order} != {EXPECTED_ORDER}{NC}")

    if raw_ok:
        print(f"{Y}[{NAME}] RAW also correct (OpenRouter handled it "
              f"transparently){NC}")
    else:
        print(f"{Y}[{NAME}] RAW wrong — confirms why we need WRAP "
              f"(got {raw_order}){NC}")

    sys.exit(0 if wrap_ok else 1)


if __name__ == "__main__":
    main()
