#!/usr/bin/env python3
"""Re-run the judge over an existing results_*.json file, using plain text
instead of JSON-mode (which some OpenRouter providers don't honor).
"""
import json
import os
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def _load_dotenv(p: Path):
    if not p.exists():
        return 0
    n = 0
    for line in p.read_text().splitlines():
        s = line.strip()
        if not s or s.startswith("#") or "=" not in s:
            continue
        k, v = s.split("=", 1); k = k.strip(); v = v.strip()
        if v.startswith(("'", '"')):
            q = v[0]; end = v.find(q, 1)
            if end != -1: v = v[1:end]
        else:
            for i, ch in enumerate(v):
                if ch == "#" and i > 0 and v[i-1] in " \t": v = v[:i]; break
            v = v.strip()
        if k and k not in os.environ: os.environ[k] = v; n += 1
    return n


for p in [Path.home()/"Documents/qifei/config/.env", Path.home()/".config/shore/.env"]:
    if _load_dotenv(p): break

key = os.environ.get("OPENROUTER_SHORE_PRIMARY") or os.environ.get("OPENROUTER_API_KEY")

from openai import OpenAI
client = OpenAI(api_key=key, base_url="https://openrouter.ai/api/v1")


JUDGE_PROMPT = """A memory system was tested on this scenario:
- Alex's cat Whiskers has died. This is established by the memory history.
- Alex asked: "{query}"

The system responded:
"{response}"

Classify the response as exactly one of these verdicts:
- PASS          — correctly acknowledges Whiskers has died
- FAIL-alive    — describes Whiskers as alive or currently doing things
- FAIL-confused — mixes alive/dead states ambiguously, or confuses with another cat
- FAIL-evasive  — says "I don't know" or similar when death info IS retrievable

Output exactly this format, on two lines:
VERDICT: <one-of-above>
REASON: <one short sentence>"""


def judge(query, response):
    r = client.chat.completions.create(
        model="anthropic/claude-haiku-4-5",
        messages=[{"role": "user", "content": JUDGE_PROMPT.format(query=query, response=response)}],
        temperature=0.0,
    )
    text = r.choices[0].message.content.strip()
    m_v = re.search(r"VERDICT:\s*(\S+)", text)
    m_r = re.search(r"REASON:\s*(.+)", text)
    return {
        "verdict": m_v.group(1) if m_v else "PARSE-ERROR",
        "reason": m_r.group(1).strip() if m_r else text[:200],
    }


def main():
    path = HERE / (sys.argv[1] if len(sys.argv) > 1 else "results_mem0.json")
    data = json.loads(path.read_text())
    for item in data:
        v = judge(item["query"], item["response"])
        item["verdict"] = v
    path.write_text(json.dumps(data, indent=2, default=str))

    print(f"\n{'='*80}\nSUMMARY (rejudge of {path.name})\n{'='*80}")
    for item in data:
        v = item["verdict"]
        print(f"  Level {item['level']:<2} {v['verdict']:<16} {item['name']}")
        print(f"             {v['reason'][:120]}")


if __name__ == "__main__":
    main()
