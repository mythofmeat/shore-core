#!/usr/bin/env python3
"""
Auto-memory gate prompt evaluation — multi-model bake-off.

For each labeled turn in labels.jsonl:
  1. Reconstruct a 36-turn rolling window ending just before that turn
  2. Build a list of long-term memory topics (session summaries for sessions
     whose turns are entirely outside the window)
  3. Call the gate prompt against each model in MODELS via OpenRouter
  4. Compare fire/no-fire decision against hand label

Reports precision/recall/F1/cost/time per model + side-by-side table.
Does NOT touch the real daemon/DB.
"""

import json
import os
import sys
import time
from pathlib import Path
from urllib import request, error

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent
DATASET = REPO / "shore-daemon/tests/data/locomo10.json"
LABELS = ROOT / os.environ.get("LABELS", "labels.jsonl")

WINDOW_SIZE = int(os.environ.get("WINDOW_SIZE", "12"))
CONV_ID = os.environ.get("CONV_ID", "conv-26")

# Per-model sampling config. Keys omitted fall back to provider default.
# "limit": cap turn count for this model (None = all labels).
MODEL_CONFIGS = {
    "x-ai/grok-4.1-fast": {"temperature": 0.2},
    "moonshotai/kimi-k2.5": {
        "temperature": 1.0,
        "top_p": 0.95,
        "extra_body": {"thinking": {"type": "disabled"}},
        "limit": 10,
    },
    "google/gemini-3.1-flash-lite-preview": {"temperature": 0.2},
    "google/gemma-4-31b-it": {"temperature": 1.0},
    "minimax/minimax-m2.7": {"temperature": 0.2},
}

# Models to run in this invocation. Override with MODELS env var.
DEFAULT_MODELS = [
    "google/gemma-4-31b-it",
]
MODELS = [m.strip() for m in os.environ.get("MODELS", ",".join(DEFAULT_MODELS)).split(",") if m.strip()]

API_KEY = os.environ.get("OPENROUTER_SHORE_TEST")
if not API_KEY:
    print("ERROR: OPENROUTER_SHORE_TEST not set", file=sys.stderr)
    sys.exit(1)


def load_conversation():
    data = json.loads(DATASET.read_text())
    conv = next(c for c in data if c["sample_id"] == CONV_ID)

    flat = []
    session_keys = sorted(
        (k for k in conv["conversation"]
         if k.startswith("session_")
         and not k.endswith("_date_time")
         and not k.endswith("_summary")),
        key=lambda s: int(s.split("_")[1]),
    )
    for k in session_keys:
        sid = int(k.split("_")[1])
        for t in conv["conversation"][k]:
            flat.append({
                "session": sid,
                "dia_id": t.get("dia_id"),
                "speaker": t.get("speaker"),
                "text": t.get("text", ""),
                "global_pos": len(flat),
            })

    summaries = {}
    for k, v in conv.get("session_summary", {}).items():
        if k.endswith("_summary") and k.startswith("session_"):
            try:
                sid = int(k.split("_")[1])
                summaries[sid] = v
            except ValueError:
                pass

    return flat, summaries


def build_window(flat, pos, size=WINDOW_SIZE):
    start = max(0, pos - size)
    return flat[start:pos]


def sessions_in_window(window):
    return {t["session"] for t in window}


def build_memory_topics(summaries, window_sessions, current_session):
    """Only include sessions older than the earliest one in window."""
    earliest_in_window = min(window_sessions) if window_sessions else current_session
    topics = []
    for sid in sorted(summaries.keys()):
        if sid < earliest_in_window:
            summary = summaries[sid].strip()
            short = (summary[:280] + "…") if len(summary) > 280 else summary
            topics.append(f"[session {sid}] {short}")
    return topics


def format_window(window):
    return "\n".join(f"{t['speaker']}: {t['text']}" for t in window)


SYSTEM_PROMPT = """You are a memory gate for a conversational character.

You see (a) a rolling conversation window of the most recent turns, (b) a list of
long-term memory topics — session summaries — that are NOT in the window, and (c) a
new incoming message the character is about to respond to.

Your job: decide whether to inject long-term memory content into the character's
context *before* it responds, so the response reflects the character's full history
instead of just the recent window.

## Fire the gate in ANY of these cases

1. EXPLICIT DISTANT CALLBACK — the message names or alludes to a specific past event,
   object, or statement ("last year", "remember when", "that book you recommended",
   "the support group you mentioned", "when we went camping"). Fire; inject the
   referenced content.

2. QUESTION NEEDING STATEFUL KNOWLEDGE — the message asks the character something
   whose answer requires prior conversation content ("what made you decide to X?",
   "how's the adoption going?", "tell me more about your community", "why is it
   special to you?"). If the answer lives across multiple prior sessions, fire.

3. ARC-REFLECTIVE CONTINUATION — the message advances a multi-session arc AND
   reflects on its history. The message must do at least one of: (a) explicitly
   acknowledge the arc's duration or growth ("a long process", "been working on",
   "how far I've come"); (b) mark a milestone in the arc ("I finally got approved",
   "I passed the interview"); (c) ask about the arc's state across time ("how's
   the journey affected your relationships?"); (d) return to the arc after a visible
   break ("I had a setback and took time off"). A surface-level mention of an
   arc-adjacent topic is NOT enough by itself — see the no-fire list below.

4. OBLIQUE REFERENCE TO SHARED HISTORY — the message uses language that implies
   shared memory without naming it explicitly ("I wish I had known you back then",
   "you've always been so supportive", "you know how I feel about X", "our gang",
   "that thing we did"). Fire so the character can respond with the right warmth
   or specificity.

5. EXPLICIT CATCH-UP OPENER — an opener whose PURPOSE is to ask the character for
   updates: "long time no see, what's new?", "since we last talked", "anything new
   with you?". Fire. Do NOT treat a narrative opener ("Hey, that roadtrip was
   insane!") as a catch-up — that is a fresh event being narrated, not a request
   for updates.

## Do NOT fire in any of these cases

- FILLER / AGREEMENT / SENTIMENT: "cool", "thanks", "totally agree", "sounds great",
  "life is too short", "family is everything". Even if the conversation is about
  an arc topic, an agreement or generic reflection does not need memory.

- COMPLIMENT OR REACTION ON AN ARC TOPIC: "That bowl is gorgeous!", "Can't wait to
  see your pottery!", "Your creativity really shines" — the speaker is reacting
  to something just shown/said, not engaging with the arc's history. No fire.

- FRESH EVENT NARRATION: the speaker is telling a brand-new story with no
  callbacks, even if the new event touches a recurring hobby ("I took the kids to
  a pottery workshop last Friday"). No fire — there is nothing in memory that
  changes the response.

- MECHANICAL REPLY: "Yes, I made it in class yesterday" in response to "did you
  make it?". No fire.

- IN-WINDOW CONTINUATION: topic originated and is still active within the current
  window, and the message doesn't reach for anything older ("Volunteering is a
  great way to meet people" in response to a volunteering story told earlier in
  the same session). No fire.

- SUPPORTIVE STATEMENT / VALIDATION: the speaker validates, affirms, or
  encourages the listener's current activity without asking anything. Examples:
  "That's awesome, you're so kind for doing this"; "Volunteering is a great way
  to meet people. Creating community is so important"; "You should be proud of
  yourself". These are declarative affirmations, NOT reflection-asking questions
  — they end in a period, not a question mark, and they do not demand any
  stateful recall. No fire.

  WARNING: Do not mistake a supportive statement for a reflection-asking question.
  A statement ends with a period and offers the speaker's own view. A question
  ends with a question mark and asks the listener to recall or reflect. Only the
  latter is a fire trigger.

## The binding question before firing

Before you output fire=true, ask: *would the character's response be meaningfully
worse if I did not inject this memory?*

- If yes — the character would sound amnesiac, miss a callback, skip over a
  milestone, or respond without warranted warmth — fire.
- If no — the response works fine with just the window, and injection would only
  pad context — do not fire.

This is the single most important check. It overrides the category list above when
they conflict.

## Grounding — you are a selector, not a generator

You do NOT write summaries or prose about what happened in the past. Instead, you
select which session summaries from the memory list should be shown to the
character before it responds. The harness injects the actual summaries verbatim.

This design eliminates the possibility of hallucination — you cannot invent
content because you aren't producing any. You pick session numbers; the actual
text comes from the memory store.

Rules:
- `inject_sessions` lists the session numbers whose summaries should be surfaced
  to the character. Every entry must match a `[session N]` shown in the memory
  topics list.
- Pick only sessions whose summaries actually contain content relevant to the
  incoming message. Do not over-select; 1–3 sessions is usually enough. Bias
  toward the most informative sessions for the specific message.
- If NO session in the memory list meaningfully helps the character respond to
  the incoming message, DO NOT FIRE. Set fire=false and inject_sessions=[].
  "Better to miss a fire than to clutter the character's context with irrelevant
  summaries."
- The arc-adjacent rule still applies: if the message mentions "camping last
  year with Perseid meteors" and the memory has other camping trips, picking
  those is correct — the character gets related context even without an exact
  match. But the chosen sessions MUST actually describe camping (or whatever
  the arc is), not just be topically close.

## Output

STRICT JSON, no prose, no code fences:
{"fire": true|false, "reason": "one sentence", "inject_sessions": [int, ...], "pointers": ["topic title", ...]}

Rules:
- If fire=false: inject_sessions MUST be an empty array.
- If fire=true: inject_sessions MUST contain ≥1 session number from the memory
  topics list.
- pointers: 0-3 related topic titles the character might search for deeper
  retrieval beyond what inject_sessions surfaces.
"""


def call_gate(model, character, window_text, memory_topics, current_turn, cfg):
    memory_block = "\n".join(f"- {t}" for t in memory_topics) if memory_topics else "(none)"
    user_content = (
        f"Character: {character}\n\n"
        f"# Recent window (last {WINDOW_SIZE} turns)\n{window_text}\n\n"
        f"# Long-term memory topics (NOT in window)\n{memory_block}\n\n"
        f"# New incoming message to respond to\n{current_turn}"
    )

    payload = {
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_PROMPT},
            {"role": "user", "content": user_content},
        ],
        "response_format": {"type": "json_object"},
        "usage": {"include": True},
    }
    if "temperature" in cfg:
        payload["temperature"] = cfg["temperature"]
    if "top_p" in cfg:
        payload["top_p"] = cfg["top_p"]
    for k, v in (cfg.get("extra_body") or {}).items():
        payload[k] = v

    req = request.Request(
        "https://openrouter.ai/api/v1/chat/completions",
        data=json.dumps(payload).encode(),
        headers={
            "Authorization": f"Bearer {API_KEY}",
            "Content-Type": "application/json",
        },
    )
    t0 = time.time()
    try:
        with request.urlopen(req, timeout=120) as resp:
            body = json.loads(resp.read())
    except error.HTTPError as e:
        return None, time.time() - t0, f"HTTP {e.code}: {e.read().decode()[:300]}"
    except Exception as e:
        return None, time.time() - t0, f"{type(e).__name__}: {e}"
    elapsed = time.time() - t0

    try:
        text = body["choices"][0]["message"]["content"]
        # Some models wrap JSON in ```json ... ``` fences.
        stripped = text.strip()
        if stripped.startswith("```"):
            stripped = stripped.strip("`")
            if stripped.startswith("json\n"):
                stripped = stripped[len("json\n"):]
            elif stripped.startswith("json"):
                stripped = stripped[len("json"):]
        parsed = json.loads(stripped)
    except Exception as e:
        return None, elapsed, f"parse error: {e} — raw: {str(body)[:400]}"
    usage = body.get("usage", {})
    return {"parsed": parsed, "usage": usage}, elapsed, None


def run_model(model, labels, flat, summaries, speaker_list):
    cfg = MODEL_CONFIGS.get(model, {})
    limit = cfg.get("limit")
    if limit is not None:
        labels = labels[:limit]
    print(f"\n{'='*100}\nMODEL: {model}  cfg={ {k:v for k,v in cfg.items() if k!='extra_body'} }  extra_body={cfg.get('extra_body')}\n{'='*100}")
    tp = fp = tn = fn = 0
    total_input = 0
    total_output = 0
    total_cost = 0.0
    total_time = 0.0
    errors = 0
    citation_failures = 0  # fires rejected for citing sessions not in topics
    per_turn = []

    for i, lab in enumerate(labels):
        pos = lab["global_pos"]
        current = flat[pos]
        other = [s for s in speaker_list if s != current["speaker"]]
        responder = other[0] if other else "character"

        window = build_window(flat, pos, WINDOW_SIZE)
        win_sessions = sessions_in_window(window)
        topics = build_memory_topics(summaries, win_sessions, current["session"])
        provided_sessions = {sid for sid in summaries.keys()
                             if sid < (min(win_sessions) if win_sessions else current["session"])}
        window_text = format_window(window)

        result, elapsed, err = call_gate(
            model, responder, window_text, topics,
            f"{current['speaker']}: {current['text']}", cfg
        )
        total_time += elapsed

        if err:
            errors += 1
            print(f"[{i+1:2d}/{len(labels)}] ERR {lab['dia_id']}: {err[:160]}")
            per_turn.append({"lab": lab, "err": err})
            continue

        parsed = result["parsed"]
        usage = result["usage"]
        total_input += usage.get("prompt_tokens", 0)
        total_output += usage.get("completion_tokens", 0)
        total_cost += float(usage.get("cost", 0) or 0)

        # Selector validation. inject_sessions must be a non-empty subset of
        # the provided topics when fire=true; otherwise downgrade to no-fire.
        raw_fire = bool(parsed.get("fire"))
        inject_sessions = parsed.get("inject_sessions") or []
        try:
            inject_int = {int(s) for s in inject_sessions}
        except (TypeError, ValueError):
            inject_int = set()
        cite_invalid = raw_fire and (
            not inject_int or not inject_int.issubset(provided_sessions)
        )
        if cite_invalid:
            citation_failures += 1
            parsed["_citation_invalid"] = True
            parsed["_original_fire"] = True
            parsed["fire"] = False
            parsed["inject_sessions"] = []
        # Build the actual injection text from cited session summaries
        # (selector design — no model-generated prose).
        if parsed.get("fire") and inject_int:
            parsed["_resolved_injection"] = "\n\n".join(
                f"[session {sid}] {summaries[sid].strip()}"
                for sid in sorted(inject_int) if sid in summaries
            )
        gate_fire = bool(parsed.get("fire"))

        gt_fire = lab["should_fire"]
        match = gt_fire == gate_fire
        mark = "✓" if match else "✗"

        if gt_fire and gate_fire: tp += 1
        elif gt_fire and not gate_fire: fn += 1
        elif not gt_fire and gate_fire: fp += 1
        else: tn += 1

        tag = "CITE✗" if cite_invalid else ""
        print(f"[{i+1:2d}/{len(labels)}] {mark} {lab['dia_id']:6s} s{lab['session']:02d} gt={gt_fire!s:5s} gate={gate_fire!s:5s} {tag:6s} t={elapsed:4.1f}s | {parsed.get('reason','')[:66]}")
        per_turn.append({
            "lab": lab, "gate": parsed, "match": match,
            "usage": usage, "elapsed": elapsed,
            "citation_invalid": cite_invalid,
            "provided_sessions": sorted(provided_sessions),
        })

    total = tp + fp + tn + fn
    summary = {
        "model": model,
        "accuracy": (tp + tn) / total if total else 0.0,
        "precision": tp / (tp + fp) if (tp + fp) else 0.0,
        "recall": tp / (tp + fn) if (tp + fn) else 0.0,
        "tp": tp, "fp": fp, "tn": tn, "fn": fn,
        "errors": errors,
        "citation_failures": citation_failures,
        "input_tokens": total_input,
        "output_tokens": total_output,
        "cost_usd": total_cost,
        "wall_time_s": total_time,
    }
    summary["f1"] = (
        2 * summary["precision"] * summary["recall"] / (summary["precision"] + summary["recall"])
        if (summary["precision"] + summary["recall"]) else 0.0
    )
    print(
        f"\n  acc={summary['accuracy']:.2%} prec={summary['precision']:.2%} rec={summary['recall']:.2%} f1={summary['f1']:.2%} "
        f"| TP={tp} FP={fp} TN={tn} FN={fn} err={errors} cite✗={citation_failures} | tokens in={total_input} out={total_output} | "
        f"${summary['cost_usd']:.4f} | {summary['wall_time_s']:.1f}s"
    )
    return summary, per_turn


def main():
    flat, summaries = load_conversation()
    speakers = set()
    for t in flat:
        speakers.add(t["speaker"])
    speaker_list = sorted(speakers)

    labels = [json.loads(line) for line in LABELS.read_text().strip().split("\n")]

    print(f"Window size   : {WINDOW_SIZE} turns")
    print(f"Labels        : {len(labels)}")
    print(f"Models        : {len(MODELS)}")
    for m in MODELS:
        print(f"  - {m}")

    all_summaries = []
    all_per_turn = {}
    for model in MODELS:
        try:
            summary, per_turn = run_model(model, labels, flat, summaries, speaker_list)
            all_summaries.append(summary)
            all_per_turn[model] = per_turn
        except Exception as e:
            print(f"  FATAL on {model}: {type(e).__name__}: {e}")
            all_summaries.append({"model": model, "fatal_error": str(e)})

    # ── comparison table ────────────────────────────────────────────────
    print(f"\n\n{'='*100}\nCOMPARISON\n{'='*100}")
    hdr = f"{'model':<46} {'acc':>7} {'prec':>7} {'rec':>7} {'f1':>7} {'cost$':>8} {'time':>7} {'err':>4}"
    print(hdr)
    print("-" * len(hdr))
    for s in all_summaries:
        if "fatal_error" in s:
            print(f"{s['model']:<46}  FATAL: {s['fatal_error'][:50]}")
            continue
        print(
            f"{s['model']:<46} "
            f"{s['accuracy']:>6.2%} {s['precision']:>6.2%} {s['recall']:>6.2%} {s['f1']:>6.2%} "
            f"{s['cost_usd']:>8.4f} {s['wall_time_s']:>6.1f}s {s['errors']:>4}"
        )

    out_name = os.environ.get("OUT", "results.json")
    out = ROOT / out_name
    out.write_text(json.dumps({
        "window_size": WINDOW_SIZE,
        "conv_id": CONV_ID,
        "n_labels": len(labels),
        "summaries": all_summaries,
        "per_turn": all_per_turn,
    }, indent=2, default=str))
    print(f"\nFull results → {out}")


if __name__ == "__main__":
    main()
