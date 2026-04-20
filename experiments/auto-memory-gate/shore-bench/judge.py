#!/usr/bin/env python3
"""
Standalone judge — reads {question, ground_truth, answer} JSON from stdin,
writes {verdict, reason} JSON to stdout. Calls Haiku via OpenRouter directly.

Kept separate from the Shore benchmark driver so the judge model/prompt can
evolve independently.
"""

import json
import os
import sys
import time
from urllib import request, error

API_KEY = os.environ.get("OPENROUTER_SHORE_TEST")
if not API_KEY:
    print(json.dumps({"verdict": "error", "reason": "OPENROUTER_SHORE_TEST not set"}))
    sys.exit(0)

MODEL = os.environ.get("BENCH_JUDGE_MODEL", "anthropic/claude-haiku-4-5")

SYSTEM = """You evaluate whether a character's answer captures the facts in a ground-truth answer to a factual recall question.

Classify the answer as:
- "correct": contains all essential facts from the ground truth, even if phrased differently
- "partial": contains SOME but not all essential facts
- "wrong": does not capture the essential facts (including "not available" refusals, unless the ground truth is explicitly unanswerable)

Output STRICT JSON only:
{"verdict": "correct"|"partial"|"wrong", "reason": "one short sentence"}"""


def main():
    payload_in = json.load(sys.stdin)
    q = payload_in.get("question", "")
    gt = payload_in.get("ground_truth", "")
    ans = payload_in.get("answer", "")

    user = f"Question: {q}\nGround truth: {gt}\nCharacter's answer: {ans}\n"
    body = {
        "model": MODEL,
        "messages": [
            {"role": "system", "content": SYSTEM},
            {"role": "user", "content": user},
        ],
        "temperature": 0.2,
        "response_format": {"type": "json_object"},
    }
    req = request.Request(
        "https://openrouter.ai/api/v1/chat/completions",
        data=json.dumps(body).encode(),
        headers={"Authorization": f"Bearer {API_KEY}", "Content-Type": "application/json"},
    )
    last_err = None
    for attempt in range(3):
        try:
            with request.urlopen(req, timeout=60) as resp:
                resp_body = json.loads(resp.read())
            text = (resp_body.get("choices", [{}])[0].get("message", {}) or {}).get("content")
            if not text:
                last_err = "empty content"
                continue
            # Haiku sometimes wraps JSON in ```json``` fences even in json_object mode.
            stripped = text.strip()
            if stripped.startswith("```"):
                stripped = stripped.lstrip("`")
                if stripped.lower().startswith("json"):
                    stripped = stripped[4:]
                if stripped.endswith("```"):
                    stripped = stripped[:-3]
                stripped = stripped.strip()
            parsed = json.loads(stripped)
            if parsed.get("verdict") not in ("correct", "partial", "wrong"):
                parsed["verdict"] = "wrong"
            print(json.dumps(parsed))
            return
        except error.HTTPError as e:
            last_err = f"HTTP {e.code}: {e.read().decode()[:200]}"
        except Exception as e:
            last_err = f"{type(e).__name__}: {e}"
        time.sleep(1)
    print(json.dumps({"verdict": "error", "reason": last_err or "unknown"}))


if __name__ == "__main__":
    main()
