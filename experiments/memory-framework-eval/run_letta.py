#!/usr/bin/env python3
"""Run the Whiskers scenarios through Letta.

Mirrors run_mem0.py — same scenarios, same judge. For each scenario:
  1. Create a fresh Letta agent with a simple character prompt.
  2. Send each memory as a user message so Letta's agent loop decides what
     to archive/supersede (the extraction-at-ingest equivalent of mem0).
  3. Ask the final query as a user message and capture the agent's reply.
  4. Judge the reply against the same PASS / FAIL-{alive,confused,evasive}
     rubric used for mem0.

Requires the Letta server to be running (see start_letta_stack.py).
"""
import json
import os
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from scenarios import SCENARIOS


def load_dotenv_into_current(path: Path) -> int:
    if not path.exists():
        return 0
    n = 0
    for line in path.read_text().splitlines():
        s = line.strip()
        if not s or s.startswith("#") or "=" not in s:
            continue
        k, v = s.split("=", 1)
        k = k.strip(); v = v.strip()
        if v.startswith(("'", '"')):
            q = v[0]; end = v.find(q, 1)
            if end != -1: v = v[1:end]
        else:
            for i, ch in enumerate(v):
                if ch == "#" and i > 0 and v[i-1] in " \t":
                    v = v[:i]; break
            v = v.strip()
        if k and k not in os.environ:
            os.environ[k] = v; n += 1
    return n


for _p in (Path.home()/"Documents/qifei/config/.env", Path.home()/".config/shore/.env"):
    if load_dotenv_into_current(_p):
        break

OR_KEY = os.environ.get("OPENROUTER_SHORE_PRIMARY") or \
         os.environ.get("OPENROUTER_SHORE_TOOL") or \
         os.environ.get("OPENROUTER_API_KEY")
if not OR_KEY:
    sys.exit("Missing OpenRouter API key.")


LETTA_URL = os.environ.get("LETTA_URL", "http://127.0.0.1:8283")

# Letta handles are of the form `<provider>/<model>`. See
# /v1/models/ and /v1/models/embedding on the running server for choices.
LLM_HANDLE = os.environ.get("LETTA_LLM_HANDLE", "openrouter/anthropic/claude-haiku-4.5")
EMBED_HANDLE = os.environ.get("LETTA_EMBED_HANDLE", "openai/text-embedding-3-small")

CHARACTER_SYSTEM = (
    "You are a close friend of Alex. You have a personal memory of your "
    "conversations with them. You respond warmly and naturally in 1–3 short "
    "sentences, in first person. Do not mention that you are looking up "
    "memories; just respond as someone who remembers. Use your archival "
    "memory tools to record and recall anything about Alex's life."
)


def _client():
    from letta_client import Letta
    return Letta(base_url=LETTA_URL)


def _extract_assistant_text(messages) -> str:
    """Letta's agent loop returns a list of messages per user turn — tool
    calls, thoughts, and at most one assistant message. We want only the
    final assistant content."""
    out = []
    for m in messages:
        mt = getattr(m, "message_type", None) or (m.get("message_type") if isinstance(m, dict) else None)
        if mt == "assistant_message":
            content = getattr(m, "content", None) or (m.get("content") if isinstance(m, dict) else None)
            if isinstance(content, list):
                # content may be a list of content blocks
                for block in content:
                    text = getattr(block, "text", None) or (block.get("text") if isinstance(block, dict) else None)
                    if text:
                        out.append(text)
            elif isinstance(content, str):
                out.append(content)
    return "\n".join(out).strip()


def run_letta_scenario(scenario):
    """Create an agent, ingest memories as user turns, ask the query."""
    client = _client()

    agent = client.agents.create(
        name=f"whiskers-l{scenario['level']}-{int(time.time())}",
        system=CHARACTER_SYSTEM,
        model=LLM_HANDLE,
        embedding=EMBED_HANDLE,
        include_base_tools=True,
    )

    ingested = []
    for i, text in enumerate(scenario["memories"], 1):
        resp = client.agents.messages.create(
            agent_id=agent.id,
            messages=[{"role": "user", "content": text}],
        )
        ingested.append({
            "input": text,
            "assistant": _extract_assistant_text(resp.messages),
        })

    query_resp = client.agents.messages.create(
        agent_id=agent.id,
        messages=[{"role": "user", "content": scenario["query"]}],
    )
    response = _extract_assistant_text(query_resp.messages)

    # Grab archival memory for debugging: what did Letta decide to store?
    try:
        passages = client.agents.passages.list(agent_id=agent.id, limit=50)
        archival_items = [
            {"text": getattr(p, "text", None) or (p.get("text") if isinstance(p, dict) else str(p))}
            for p in passages
        ]
    except Exception as e:
        archival_items = [{"error": str(e)}]

    return {
        "agent_id": agent.id,
        "ingested": ingested,
        "archival": archival_items,
        "response": response,
    }


def judge(scenario, response):
    from openai import OpenAI
    c = OpenAI(api_key=OR_KEY, base_url="https://openrouter.ai/api/v1")
    prompt = (
        f"A memory system was tested on this scenario:\n"
        f"- Alex's cat Whiskers has died. This is established by the memory history.\n"
        f"- Alex asked: \"{scenario['query']}\"\n\n"
        f"The system responded:\n\"{response}\"\n\n"
        f"Classify the response as exactly one of these verdicts:\n"
        f"- PASS          — correctly acknowledges Whiskers has died\n"
        f"- FAIL-alive    — describes Whiskers as alive or currently doing things\n"
        f"- FAIL-confused — mixes alive/dead states ambiguously, or confuses with another cat\n"
        f"- FAIL-evasive  — says \"I don't know\" or similar when death info IS retrievable\n\n"
        f"Output exactly this format, on two lines:\n"
        f"VERDICT: <one-of-above>\n"
        f"REASON: <one short sentence>"
    )
    r = c.chat.completions.create(
        model="anthropic/claude-haiku-4-5",
        messages=[{"role": "user", "content": prompt}],
        temperature=0.0,
    )
    text = r.choices[0].message.content.strip()
    import re
    m_v = re.search(r"VERDICT:\s*(\S+)", text)
    m_r = re.search(r"REASON:\s*(.+)", text)
    return {
        "verdict": m_v.group(1) if m_v else "PARSE-ERROR",
        "reason": m_r.group(1).strip() if m_r else text[:200],
    }


def main():
    results = []
    for scenario in SCENARIOS:
        print(f"\n{'='*80}\nLevel {scenario['level']}: {scenario['name']}\n{'='*80}")
        print(f"Query: {scenario['query']}")
        try:
            ran = run_letta_scenario(scenario)
        except Exception as e:
            print(f"  ERROR: {e}")
            results.append({
                "level": scenario["level"],
                "name": scenario["name"],
                "query": scenario["query"],
                "error": str(e),
            })
            continue

        v = judge(scenario, ran["response"])
        print(f"\nArchival ({len(ran['archival'])} items):")
        for item in ran["archival"][:10]:
            t = item.get("text") or item.get("error") or ""
            print(f"  - {t[:120]}")
        print(f"\nResponse:\n  {ran['response']}")
        print(f"\nVerdict: {v['verdict']}  —  {v['reason']}")

        results.append({
            "level": scenario["level"],
            "name": scenario["name"],
            "query": scenario["query"],
            "agent_id": ran["agent_id"],
            "ingested": ran["ingested"],
            "archival": ran["archival"],
            "response": ran["response"],
            "verdict": v,
        })

    print(f"\n{'='*80}\nSUMMARY\n{'='*80}")
    for r in results:
        v = r.get("verdict", {}).get("verdict", "ERROR")
        print(f"  Level {r['level']}: {v:<18} {r['name']}")

    out = HERE / "results_letta.json"
    out.write_text(json.dumps(results, indent=2, default=str))
    print(f"\nSaved → {out}")


if __name__ == "__main__":
    main()
