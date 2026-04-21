#!/usr/bin/env python3
"""Run the Whiskers scenarios through mem0.

For each scenario:
  1. Create a fresh mem0 Memory instance with a unique user_id.
  2. Ingest each memory as a user message (mem0's add() triggers fact
     extraction via an LLM).
  3. Run mem0.search() with the query.
  4. Pass the retrieved facts + query to a synthesizer LLM with a simple
     character prompt, so we observe end-user-visible behavior, not just
     retrieval ranking.
  5. Print retrieval, response, and a judge verdict.

Judge is a separate LLM call that classifies the response as:
  - PASS: correctly acknowledges Whiskers has died
  - FAIL-alive: describes Whiskers as alive or performing current activities
  - FAIL-confused: mentions both states ambiguously, or refers to a different cat
  - FAIL-evasive: says "I don't know" when death information IS retrievable
"""

import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from scenarios import SCENARIOS


def load_dotenv_into_current(path: Path) -> int:
    """Same loader used by the shore-bench scripts — strips inline comments."""
    if not path.exists():
        return 0
    n = 0
    for line in path.read_text().splitlines():
        s = line.strip()
        if not s or s.startswith("#") or "=" not in s:
            continue
        k, v = s.split("=", 1)
        k = k.strip()
        v = v.strip()
        if v.startswith(("'", '"')):
            quote = v[0]
            end = v.find(quote, 1)
            if end != -1:
                v = v[1:end]
        else:
            for i, ch in enumerate(v):
                if ch == "#" and i > 0 and v[i - 1] in " \t":
                    v = v[:i]
                    break
            v = v.strip()
        if k and k not in os.environ:
            os.environ[k] = v
            n += 1
    return n


for _p in (Path.home() / "Documents/qifei/config/.env", Path.home() / ".config/shore/.env"):
    if load_dotenv_into_current(_p):
        break


# mem0 config: use OpenRouter as an OpenAI-compatible endpoint for both
# LLM (fact extraction + query rewriting) and embeddings.
# mem0 expects OPENAI_API_KEY + OPENAI_BASE_URL set, or explicit config.
# We pick Haiku for the extraction LLM (cheap, fast) and OpenAI text-embed-3
# via OpenRouter if available — else fall back to Qwen via OpenRouter.

OR_KEY = os.environ.get("OPENROUTER_SHORE_PRIMARY") or \
         os.environ.get("OPENROUTER_SHORE_TOOL") or \
         os.environ.get("OPENROUTER_API_KEY")
OR_EMBED_KEY = os.environ.get("OPENROUTER_SHORE_EMBEDDING") or OR_KEY

if not OR_KEY:
    sys.exit("Missing OpenRouter API key. Set OPENROUTER_SHORE_PRIMARY or OPENROUTER_API_KEY.")

MEM0_CONFIG = {
    "llm": {
        "provider": "openai",
        "config": {
            "model": "anthropic/claude-haiku-4-5",
            "api_key": OR_KEY,
            "openai_base_url": "https://openrouter.ai/api/v1",
            "temperature": 0.1,
        },
    },
    "embedder": {
        "provider": "openai",
        "config": {
            "model": "openai/text-embedding-3-small",
            "api_key": OR_EMBED_KEY,
            "openai_base_url": "https://openrouter.ai/api/v1",
        },
    },
    "vector_store": {
        "provider": "qdrant",
        "config": {
            "collection_name": "mem0_whiskers_test",
            "path": str(HERE / "mem0_store"),
            "embedding_model_dims": 1536,
            "on_disk": True,
        },
    },
}


def run_mem0_scenario(scenario):
    from mem0 import Memory
    user_id = f"alex-level-{scenario['level']}"

    # Fresh per-scenario collection by namespacing the collection name.
    cfg = dict(MEM0_CONFIG)
    cfg["vector_store"] = dict(MEM0_CONFIG["vector_store"])
    cfg["vector_store"]["config"] = dict(MEM0_CONFIG["vector_store"]["config"])
    cfg["vector_store"]["config"]["collection_name"] = f"whiskers_l{scenario['level']}"

    m = Memory.from_config(cfg)
    # Clean any stale data from a prior run with the same user_id.
    try:
        m.delete_all(user_id=user_id)
    except Exception:
        pass

    ingested = []
    for i, text in enumerate(scenario["memories"], 1):
        result = m.add(
            messages=[{"role": "user", "content": text}],
            user_id=user_id,
        )
        ingested.append({"input": text, "add_result": result})

    # Search. mem0 2.0 requires filters={'user_id': ...} instead of top-level kwarg.
    search_result = m.search(
        query=scenario["query"],
        filters={"user_id": user_id},
        limit=10,
    )
    return {
        "ingested": ingested,
        "search_result": search_result,
    }


def synthesize_response(scenario, retrieval):
    """Ask an LLM to respond as a character given retrieval + query."""
    from openai import OpenAI
    client = OpenAI(api_key=OR_KEY, base_url="https://openrouter.ai/api/v1")

    # Extract the fact-like strings from mem0's search result
    results = retrieval.get("results", retrieval) if isinstance(retrieval, dict) else retrieval
    if isinstance(results, dict) and "results" in results:
        results = results["results"]
    if not isinstance(results, list):
        results = []

    memory_lines = []
    for r in results:
        if isinstance(r, dict):
            memory_lines.append(f"- {r.get('memory', r.get('text', str(r)))}")
        else:
            memory_lines.append(f"- {r}")
    memory_block = "\n".join(memory_lines) if memory_lines else "(no memories returned)"

    system = (
        "You are a close friend of Alex. You have a personal memory of your "
        "conversations with them. You respond warmly and naturally in a single "
        "short paragraph — 1–3 sentences. You speak in first person. Do not "
        "mention that you are looking up memories; just respond as someone who "
        "remembers."
    )
    user = (
        f"Relevant memories from my past conversations with Alex:\n"
        f"{memory_block}\n\n"
        f"Alex just asked: \"{scenario['query']}\"\n\n"
        f"Respond naturally."
    )

    resp = client.chat.completions.create(
        model="anthropic/claude-haiku-4-5",
        messages=[
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        temperature=0.7,
    )
    return resp.choices[0].message.content.strip()


def judge(scenario, response):
    """Classify the response against scenario['expected']."""
    from openai import OpenAI
    client = OpenAI(api_key=OR_KEY, base_url="https://openrouter.ai/api/v1")

    prompt = (
        f"A memory system was tested with this scenario:\n"
        f"- Alex's cat Whiskers has died (this is established by the memory history).\n"
        f"- Alex asked: \"{scenario['query']}\"\n"
        f"- Expected behavior: {scenario['expected']}\n\n"
        f"The system responded:\n\"{response}\"\n\n"
        f"Classify the response as exactly one of:\n"
        f"  PASS           — correctly acknowledges Whiskers has died\n"
        f"  FAIL-alive     — describes Whiskers as alive or doing current activities\n"
        f"  FAIL-confused  — mixes alive/dead states ambiguously, or confuses with another cat\n"
        f"  FAIL-evasive   — says 'I don't know' or similar when death info IS retrievable\n\n"
        f"Respond with JSON only: {{\"verdict\": \"...\", \"reason\": \"one sentence\"}}"
    )

    r = client.chat.completions.create(
        model="anthropic/claude-haiku-4-5",
        messages=[{"role": "user", "content": prompt}],
        temperature=0.0,
        response_format={"type": "json_object"},
    )
    try:
        return json.loads(r.choices[0].message.content.strip())
    except Exception as e:
        return {"verdict": "JUDGE-ERROR", "reason": str(e)}


def main():
    results = []
    for scenario in SCENARIOS:
        print(f"\n{'='*80}")
        print(f"Level {scenario['level']}: {scenario['name']}")
        print(f"{'='*80}")
        print(f"Query: {scenario['query']}")

        retrieval = run_mem0_scenario(scenario)
        response = synthesize_response(scenario, retrieval["search_result"])
        verdict = judge(scenario, response)

        # Show what mem0 returned
        sr = retrieval["search_result"]
        if isinstance(sr, dict) and "results" in sr:
            sr = sr["results"]
        print(f"\nmem0 retrieved {len(sr) if isinstance(sr, list) else '?'} memories:")
        if isinstance(sr, list):
            for r in sr[:10]:
                mem = r.get("memory", r.get("text", "?")) if isinstance(r, dict) else str(r)
                score = r.get("score", "?") if isinstance(r, dict) else ""
                print(f"  [{score}] {mem[:120]}")

        print(f"\nResponse:\n  {response}")
        print(f"\nVerdict: {verdict.get('verdict')}  —  {verdict.get('reason')}")

        results.append({
            "level": scenario["level"],
            "name": scenario["name"],
            "query": scenario["query"],
            "ingested": retrieval["ingested"],
            "retrieval": sr if isinstance(sr, list) else [],
            "response": response,
            "verdict": verdict,
        })

    # Summary
    print(f"\n{'='*80}\nSUMMARY\n{'='*80}")
    for r in results:
        v = r["verdict"].get("verdict", "?")
        print(f"  Level {r['level']}: {v:<18} {r['name']}")

    out_path = HERE / "results_mem0.json"
    out_path.write_text(json.dumps(results, indent=2, default=str))
    print(f"\nSaved → {out_path}")


if __name__ == "__main__":
    main()
