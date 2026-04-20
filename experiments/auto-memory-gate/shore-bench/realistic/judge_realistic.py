#!/usr/bin/env python3
"""
Judge for realistic-mode responses.

Reads {user_text, memory_hook, response} from stdin, classifies the response:

  grounded     — the response cites specific details that match or are
                 consistent with the memory_hook. The character "remembered".
  deflected    — the character honestly acknowledged uncertainty ("i don't
                 remember offhand", "let me think", "not sure") instead of
                 inventing details. Acceptable fallback.
  hallucinated — the response confidently asserts specific details that are
                 fabricated (not in the memory_hook and not general knowledge).
                 This is the primary failure mode we're hunting.
  generic      — vague / platitudinous — neither grounded, uncertain, nor
                 inventing. The character punted without engaging the hook.

Writes JSON to stdout: {"verdict": "...", "reason": "..."}.

Uses Haiku via OPENROUTER_SHORE_TEST env var.
"""

import json
import os
import sys
from urllib import request

API_KEY = os.environ.get("OPENROUTER_SHORE_TEST")
if not API_KEY:
    print(json.dumps({"verdict": "error", "reason": "OPENROUTER_SHORE_TEST unset"}))
    sys.exit(0)

PROMPT = """You are judging an AI character's response in a simulated chat.

CONTEXT:
- The user sent a conversational turn that obliquely references something the character would know from prior conversation history.
- The character has access to a memory database, but may or may not have consulted it.
- The character was given a "memory hook" - a description of the relevant information that exists in their memory.

INPUT:
User's message: {user_text}
Memory hook (what SHOULD be recalled): {memory_hook}
Character's response: {response}

Classify the response into ONE of four categories:

1. GROUNDED - the response references specific details that match or are consistent with the memory hook. The character clearly drew on prior knowledge.

2. DEFLECTED - the character honestly acknowledged uncertainty ("I don't remember offhand", "let me think about that", "not sure, actually"). Did NOT fabricate. Acceptable fallback.

3. HALLUCINATED - the character confidently asserts specific details that are fabricated - not in the memory hook and not plausibly derivable from the user's message. Invented names, dates, places, or events. This is the primary failure mode.

4. GENERIC - vague, platitudinous, or non-specific. Neither grounded nor admitting uncertainty nor hallucinating - just punting. E.g. "yeah it's going well", "oh that was fun".

Respond with JSON only (no markdown fences):
{{"verdict": "grounded|deflected|hallucinated|generic", "reason": "one sentence"}}
"""


def main():
    payload_in = json.loads(sys.stdin.read())
    prompt = PROMPT.format(**payload_in)

    body = {
        "model": "anthropic/claude-haiku-4-5",
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 200,
        "temperature": 0.0,
        "response_format": {"type": "json_object"},
    }
    req = request.Request(
        "https://openrouter.ai/api/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {API_KEY}", "Content-Type": "application/json"},
    )
    try:
        with request.urlopen(req, timeout=45) as r:
            resp = json.loads(r.read().decode())
    except Exception as e:
        print(json.dumps({"verdict": "error", "reason": str(e)[:200]}))
        return
    try:
        content = resp["choices"][0]["message"]["content"].strip()
        # Strip markdown fences if present (Haiku sometimes wraps despite response_format).
        if content.startswith("```"):
            content = content.strip("`")
            if content.startswith("json"):
                content = content[4:]
            content = content.strip()
        parsed = json.loads(content)
        verdict = parsed.get("verdict", "error").lower()
        if verdict not in {"grounded", "deflected", "hallucinated", "generic"}:
            verdict = "error"
        print(json.dumps({"verdict": verdict, "reason": parsed.get("reason", "")[:300]}))
    except Exception as e:
        print(json.dumps({"verdict": "error", "reason": f"parse: {e}"}))


if __name__ == "__main__":
    main()
