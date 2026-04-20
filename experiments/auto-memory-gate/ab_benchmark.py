#!/usr/bin/env python3
"""
Phase 2 A/B benchmark: auto-memory-gate vs. character-flies-blind.

For each sampled LoCoMo QA pair:
  1. Place the character at the END of the conversation (last turn of last session).
  2. Arm A: character sees last W turns + the question, no memory.
  3. Arm B: gate runs on (last W turns, session summaries, question). If fires,
     selected session summaries are prepended as context. Character then answers.
  4. Score both answers with token F1 against LoCoMo ground truth.

Reports: per-arm F1 overall + by category, F1 delta, fire rate, cost/latency.
"""

import json
import os
import re
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path
from urllib import request, error

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent
DATASET = REPO / "shore-daemon/tests/data/locomo10.json"

WINDOW_SIZE = int(os.environ.get("WINDOW_SIZE", "12"))
SAMPLE_PER_CAT = int(os.environ.get("SAMPLE_PER_CAT", "5"))  # 5 × 4 cats × 2 convs = 40 questions
CONVS = os.environ.get("CONVS", "conv-26,conv-50").split(",")
SEED = int(os.environ.get("SEED", "42"))

GATE_MODEL = os.environ.get("GATE_MODEL", "google/gemma-4-31b-it")
CHAR_MODEL = os.environ.get("CHAR_MODEL", "anthropic/claude-haiku-4-5")
JUDGE_MODEL = os.environ.get("JUDGE_MODEL", "google/gemma-4-31b-it")

API_KEY = os.environ.get("OPENROUTER_SHORE_TEST")
if not API_KEY:
    print("ERROR: OPENROUTER_SHORE_TEST not set", file=sys.stderr)
    sys.exit(1)


# ── LoCoMo loading / scoring ────────────────────────────────────────────────


def tokenize(s):
    return [t for t in re.split(r"[^a-z0-9]+", s.lower()) if t]


def token_f1(pred, gt):
    p, g = tokenize(pred), tokenize(gt)
    if not g and not p:
        return 1.0
    if not g or not p:
        return 0.0
    remain = Counter(g)
    matches = 0
    for t in p:
        if remain[t] > 0:
            remain[t] -= 1
            matches += 1
    if matches == 0:
        return 0.0
    precision = matches / len(p)
    recall = matches / len(g)
    return 2 * precision * recall / (precision + recall)


def score_qa(pred, qa):
    """Canonical LoCoMo token F1."""
    a = qa.get("answer")
    if a is None:
        return 0.0
    cat = qa["category"]
    if cat == 1:
        subs = [s.strip() for s in str(a).split(",") if s.strip()]
        if not subs:
            return 0.0
        return sum(token_f1(pred, s) for s in subs) / len(subs)
    if cat == 3:
        first = str(a).split(";")[0].strip()
        return token_f1(pred, first)
    if cat in (2, 4):
        return token_f1(pred, str(a))
    return 0.0


def recall_score(pred, gt):
    """Token recall only — what fraction of GT tokens appear in the answer?"""
    p, g = tokenize(pred), tokenize(gt)
    if not g:
        return 1.0 if not p else 0.0
    pset = Counter(p)
    matches = 0
    for t in g:
        if pset[t] > 0:
            pset[t] -= 1
            matches += 1
    return matches / len(g)


def recall_score_qa(pred, qa):
    a = qa.get("answer")
    if a is None:
        return 0.0
    cat = qa["category"]
    if cat == 1:
        subs = [s.strip() for s in str(a).split(",") if s.strip()]
        if not subs:
            return 0.0
        return sum(recall_score(pred, s) for s in subs) / len(subs)
    if cat == 3:
        first = str(a).split(";")[0].strip()
        return recall_score(pred, first)
    return recall_score(pred, str(a))


def substring_score(pred, gt):
    """Does pred contain gt as a substring, case-insensitive, punctuation-tolerant?"""
    def norm(s):
        return re.sub(r"[^a-z0-9\s]", "", s.lower()).strip()
    p, g = norm(pred), norm(gt)
    if not g:
        return 1.0 if not p else 0.0
    return 1.0 if g in p else 0.0


def substring_score_qa(pred, qa):
    a = qa.get("answer")
    if a is None:
        return 0.0
    cat = qa["category"]
    if cat == 1:
        subs = [s.strip() for s in str(a).split(",") if s.strip()]
        if not subs:
            return 0.0
        return sum(substring_score(pred, s) for s in subs) / len(subs)
    if cat == 3:
        first = str(a).split(";")[0].strip()
        return substring_score(pred, first)
    return substring_score(pred, str(a))


def load_conv(conv_id):
    data = json.loads(DATASET.read_text())
    conv = next(c for c in data if c["sample_id"] == conv_id)
    flat = []
    skeys = sorted(
        (k for k in conv["conversation"]
         if k.startswith("session_")
         and not k.endswith("_date_time")
         and not k.endswith("_summary")),
        key=lambda s: int(s.split("_")[1]),
    )
    for k in skeys:
        sid = int(k.split("_")[1])
        for t in conv["conversation"][k]:
            flat.append({
                "session": sid,
                "speaker": t.get("speaker"),
                "dia_id": t.get("dia_id"),
                "text": t.get("text", ""),
            })
    summaries = {}
    for k, v in conv.get("session_summary", {}).items():
        if k.startswith("session_") and k.endswith("_summary"):
            try:
                sid = int(k.split("_")[1])
                summaries[sid] = v
            except ValueError:
                pass
    speakers = [conv["conversation"].get("speaker_a"), conv["conversation"].get("speaker_b")]
    return flat, summaries, [s for s in speakers if s]


def stratified_sample(qas, per_cat, seed):
    import random
    rng = random.Random(seed)
    buckets = defaultdict(list)
    for qa in qas:
        if qa["category"] in (1, 2, 3, 4) and qa.get("answer") is not None:
            buckets[qa["category"]].append(qa)
    sampled = []
    for c in (1, 2, 3, 4):
        items = buckets.get(c, [])
        rng.shuffle(items)
        sampled.extend(items[:per_cat])
    return sampled


# ── LLM plumbing ────────────────────────────────────────────────────────────


def call_openrouter(model, messages, response_format=None, temperature=1.0, extra=None, max_retries=2):
    payload = {
        "model": model,
        "messages": messages,
        "temperature": temperature,
        "usage": {"include": True},
    }
    if response_format is not None:
        payload["response_format"] = response_format
    if extra:
        payload.update(extra)
    req = request.Request(
        "https://openrouter.ai/api/v1/chat/completions",
        data=json.dumps(payload).encode(),
        headers={"Authorization": f"Bearer {API_KEY}", "Content-Type": "application/json"},
    )
    last_err = None
    for attempt in range(max_retries + 1):
        t0 = time.time()
        try:
            with request.urlopen(req, timeout=120) as resp:
                body = json.loads(resp.read())
            elapsed = time.time() - t0
            msg = body["choices"][0]["message"]
            text = msg.get("content")
            if not text:
                last_err = "empty content"
                continue
            usage = body.get("usage", {}) or {}
            return {
                "text": text,
                "prompt_tokens": usage.get("prompt_tokens", 0),
                "completion_tokens": usage.get("completion_tokens", 0),
                "cost": float(usage.get("cost", 0) or 0),
                "elapsed": elapsed,
            }
        except error.HTTPError as e:
            last_err = f"HTTP {e.code}: {e.read().decode()[:200]}"
        except Exception as e:
            last_err = f"{type(e).__name__}: {e}"
    return {"error": last_err, "elapsed": 0.0}


# ── Gate (v7 selector) ──────────────────────────────────────────────────────


GATE_SYSTEM = """You are a memory gate for a conversational character.

You see (a) a rolling conversation window, (b) a list of long-term memory topics
as session summaries NOT in the window, and (c) a new incoming message the
character is about to respond to.

Your job: pick which session IDs should be surfaced to the character. The
harness will inject the actual session summaries verbatim. You do NOT write
prose about memory content.

Fire when the message references distant content, asks a stateful question,
continues a multi-session arc with a clear reflection/milestone, uses oblique
shared-history language, or is an explicit catch-up opener. Do NOT fire on
filler, fresh in-window topics, mechanical replies, or supportive declarative
statements with no question.

`inject_sessions` must be a non-empty subset of the session IDs in the memory
topics list whenever fire=true. If no session in the list is relevant, do NOT
fire. Be conservative.

Output STRICT JSON only:
{"fire": true|false, "reason": "one sentence", "inject_sessions": [int, ...]}"""


def run_gate(window_text, topics_text, incoming, provided_sessions):
    user = (
        f"# Recent window (last {WINDOW_SIZE} turns)\n{window_text}\n\n"
        f"# Long-term memory topics (NOT in window)\n{topics_text}\n\n"
        f"# Incoming message\n{incoming}"
    )
    r = call_openrouter(
        GATE_MODEL,
        [{"role": "system", "content": GATE_SYSTEM}, {"role": "user", "content": user}],
        response_format={"type": "json_object"},
        temperature=1.0,
    )
    if "error" in r:
        return {"fire": False, "reason": f"gate error: {r['error']}", "inject_sessions": []}, r
    try:
        parsed = json.loads(r["text"].strip())
    except Exception as e:
        return {"fire": False, "reason": f"gate parse error: {e}", "inject_sessions": []}, r
    sels = parsed.get("inject_sessions") or []
    try:
        valid = {int(s) for s in sels if int(s) in provided_sessions}
    except (TypeError, ValueError):
        valid = set()
    if parsed.get("fire") and not valid:
        parsed["fire"] = False
        parsed["inject_sessions"] = []
    else:
        parsed["inject_sessions"] = sorted(valid)
    return parsed, r


# ── Character answerer ──────────────────────────────────────────────────────


def build_char_prompt(responder, window_text, injection, question, other):
    sys = (
        "You are answering a factual recall question about a long-running conversation. "
        "Respond with the SHORTEST POSSIBLE answer — usually 1 to 5 words. "
        "Match the format of the question:\n"
        "  - 'when' → a date or year\n"
        "  - 'what did X do' → a verb phrase or short noun phrase\n"
        "  - 'what items / activities / events' → a comma-separated list of the core items only, no surrounding prose\n"
        "  - 'how long' → a duration\n"
        "  - yes/no question → 'Yes' or 'No'\n"
        "Do not include prefixes like 'Based on our conversation' or 'I think'. "
        "Do not add caveats or explanations. Just the answer tokens. "
        "If the answer is genuinely not in the provided context, respond with 'not available'."
    )
    parts = [f"# Recent conversation\n{window_text}"]
    if injection:
        parts.append(f"# Background from earlier sessions\n{injection}")
    parts.append(f"# Question\n{question}\n\nAnswer (shortest possible):")
    return sys, "\n\n".join(parts)


def answer_question(responder, window_text, injection, question, other):
    sys, user = build_char_prompt(responder, window_text, injection, question, other)
    r = call_openrouter(
        CHAR_MODEL,
        [{"role": "system", "content": sys}, {"role": "user", "content": user}],
        temperature=0.3,
    )
    if "error" in r:
        return "", r
    return r["text"].strip(), r


# ── LLM-as-judge ────────────────────────────────────────────────────────────


JUDGE_SYSTEM = """You are evaluating whether a character's answer captures the facts in a ground-truth answer to a factual recall question.

Given the question, ground truth, and the character's answer, classify the answer as:
- "correct": the answer contains all essential facts from the ground truth, even if phrased differently or surrounded by extra words
- "partial": the answer contains SOME but not all essential facts
- "wrong": the answer does not capture the essential facts (including "I don't know" style refusals, unless the ground truth is explicitly "not applicable")

Respond with STRICT JSON only:
{"verdict": "correct"|"partial"|"wrong", "reason": "one short sentence"}"""


def judge_answer(question, ground_truth, answer):
    user = (
        f"Question: {question}\n"
        f"Ground truth: {ground_truth}\n"
        f"Character's answer: {answer}\n"
    )
    r = call_openrouter(
        JUDGE_MODEL,
        [{"role": "system", "content": JUDGE_SYSTEM}, {"role": "user", "content": user}],
        response_format={"type": "json_object"},
        temperature=0.3,
    )
    if "error" in r:
        return {"verdict": "error", "reason": r.get("error", "")}, r
    try:
        parsed = json.loads(r["text"].strip())
    except Exception as e:
        return {"verdict": "parse_error", "reason": str(e)}, r
    if parsed.get("verdict") not in ("correct", "partial", "wrong"):
        parsed["verdict"] = "wrong"
    return parsed, r


# ── Main ────────────────────────────────────────────────────────────────────


def format_window(turns):
    return "\n".join(f"{t['speaker']}: {t['text']}" for t in turns)


def format_topics(summaries, exclude_sessions):
    topics = []
    for sid in sorted(summaries.keys()):
        if sid in exclude_sessions:
            continue
        s = summaries[sid].strip()
        short = (s[:280] + "…") if len(s) > 280 else s
        topics.append(f"[session {sid}] {short}")
    return topics


def format_injection(summaries, sessions):
    return "\n\n".join(
        f"[session {sid}] {summaries[sid].strip()}"
        for sid in sorted(sessions) if sid in summaries
    )


METRICS = ("f1", "recall", "substring", "judge_correct", "judge_any")


def main():
    # arm -> metric -> category -> list of scores
    scores = {arm: {m: defaultdict(list) for m in METRICS} for arm in ("A", "B")}
    fires = 0
    total_qs = 0
    total_cost = 0.0
    total_time = 0.0
    all_rows = []

    for conv_id in CONVS:
        print(f"\n{'='*100}\nCONV: {conv_id}\n{'='*100}")
        flat, summaries, speakers = load_conv(conv_id)
        data = json.loads(DATASET.read_text())
        conv = next(c for c in data if c["sample_id"] == conv_id)
        qas = stratified_sample(conv["qa"], SAMPLE_PER_CAT, SEED)

        pos = len(flat)
        window = flat[max(0, pos - WINDOW_SIZE):pos]
        win_sessions = {t["session"] for t in window}
        topics = format_topics(summaries, win_sessions)
        topics_text = "\n".join(f"- {t}" for t in topics) or "(none)"
        window_text = format_window(window)
        provided_sessions = {sid for sid in summaries.keys() if sid not in win_sessions}

        responder = speakers[0] if speakers else "the character"
        other = speakers[1] if len(speakers) > 1 else "the user"

        for i, qa in enumerate(qas):
            q = qa["question"]
            gt = str(qa.get("answer", ""))
            cat = qa["category"]
            total_qs += 1

            # Gate
            gate_result, gate_meta = run_gate(window_text, topics_text, q, provided_sessions)
            if "cost" in gate_meta:
                total_cost += gate_meta["cost"]
                total_time += gate_meta["elapsed"]
            fired = bool(gate_result.get("fire"))
            fires += int(fired)
            injection = format_injection(summaries, gate_result.get("inject_sessions", [])) if fired else ""

            # Arm A and Arm B answers
            ans_A, meta_A = answer_question(responder, window_text, "", q, other)
            ans_B, meta_B = answer_question(responder, window_text, injection, q, other)
            for m in (meta_A, meta_B):
                if "cost" in m:
                    total_cost += m["cost"]
                    total_time += m["elapsed"]

            # Judge both arms
            judge_A_res, judge_A_meta = judge_answer(q, gt, ans_A)
            judge_B_res, judge_B_meta = judge_answer(q, gt, ans_B)
            for m in (judge_A_meta, judge_B_meta):
                if "cost" in m:
                    total_cost += m["cost"]
                    total_time += m["elapsed"]

            def judge_vals(res):
                v = res.get("verdict", "wrong")
                return float(v == "correct"), float(v in ("correct", "partial"))

            judge_corr_A, judge_any_A = judge_vals(judge_A_res)
            judge_corr_B, judge_any_B = judge_vals(judge_B_res)

            row_scores = {
                "A": {
                    "f1": score_qa(ans_A, qa),
                    "recall": recall_score_qa(ans_A, qa),
                    "substring": substring_score_qa(ans_A, qa),
                    "judge_correct": judge_corr_A,
                    "judge_any": judge_any_A,
                },
                "B": {
                    "f1": score_qa(ans_B, qa),
                    "recall": recall_score_qa(ans_B, qa),
                    "substring": substring_score_qa(ans_B, qa),
                    "judge_correct": judge_corr_B,
                    "judge_any": judge_any_B,
                },
            }
            for arm in ("A", "B"):
                for m in METRICS:
                    scores[arm][m][cat].append(row_scores[arm][m])

            fire_tag = f"🔥{len(gate_result.get('inject_sessions',[]))}" if fired else "  "
            judge_mark = {
                ("correct", "correct"): "==",
                ("correct", "partial"): "↓",
                ("correct", "wrong"): "↓↓",
                ("partial", "correct"): "↑",
                ("partial", "partial"): "==",
                ("partial", "wrong"): "↓",
                ("wrong", "correct"): "↑↑",
                ("wrong", "partial"): "↑",
                ("wrong", "wrong"): "··",
            }.get((judge_A_res.get("verdict"), judge_B_res.get("verdict")), "??")
            print(f"  [c{cat}] {fire_tag} judge:{judge_A_res.get('verdict','?')[:5]}→{judge_B_res.get('verdict','?')[:5]} {judge_mark} "
                  f"f1 {row_scores['A']['f1']:.2f}→{row_scores['B']['f1']:.2f}  "
                  f"sub {row_scores['A']['substring']:.2f}→{row_scores['B']['substring']:.2f}  | {q[:56]}")

            all_rows.append({
                "conv_id": conv_id, "category": cat, "question": q, "ground_truth": gt,
                "fired": fired, "inject_sessions": gate_result.get("inject_sessions", []),
                "gate_reason": gate_result.get("reason", ""),
                "answer_A": ans_A, "answer_B": ans_B,
                "judge_A": judge_A_res, "judge_B": judge_B_res,
                "scores": row_scores,
            })

    # Aggregate report
    print("\n" + "=" * 100)
    print("AGGREGATE — Arm A (no memory) vs. Arm B (gate-injected memory)")
    print("=" * 100)
    cat_names = {1: "multi-hop", 2: "temporal", 3: "open-domain", 4: "single-hop"}
    metric_labels = {"f1": "token F1", "recall": "recall", "substring": "substring",
                     "judge_correct": "judge=correct", "judge_any": "judge≥partial"}

    # Overall per-metric table
    print(f"\n{'Metric':<15} {'Arm A':>10} {'Arm B':>10} {'Δ abs':>10} {'Δ rel':>10}")
    print("-" * 60)
    overall_summary = {}
    for m in METRICS:
        a_vals = [x for cat in scores["A"][m].values() for x in cat]
        b_vals = [x for cat in scores["B"][m].values() for x in cat]
        avg_a = sum(a_vals) / len(a_vals) if a_vals else 0
        avg_b = sum(b_vals) / len(b_vals) if b_vals else 0
        delta = avg_b - avg_a
        rel = f"{avg_b/avg_a:.2f}×" if avg_a > 0 else "—"
        overall_summary[m] = {"A": avg_a, "B": avg_b, "delta": delta}
        print(f"{metric_labels[m]:<15} {avg_a:>9.2%} {avg_b:>9.2%} {delta:>+9.2%} {rel:>10}")

    # Per-category breakdown for the most informative metric (judge_correct)
    print(f"\nJudge verdict (correct) by category:")
    print(f"{'Category':<14} {'A':>8} {'B':>8} {'Δ':>8} {'n':>4}")
    for cat in (1, 2, 3, 4):
        a = scores["A"]["judge_correct"].get(cat, [])
        b = scores["B"]["judge_correct"].get(cat, [])
        if not a:
            continue
        avg_a = sum(a) / len(a)
        avg_b = sum(b) / len(b)
        print(f"  {cat}. {cat_names[cat]:<10} {avg_a:>7.2%} {avg_b:>7.2%} {avg_b - avg_a:>+7.2%} {len(a):>4}")

    print(f"\nFire rate  : {fires}/{total_qs} = {fires/total_qs:.1%}")
    print(f"Total cost : ${total_cost:.4f}")
    print(f"Wall time  : {total_time:.1f}s")

    # Rescues and regressions per judge
    rescues = [r for r in all_rows
               if r["judge_A"].get("verdict") == "wrong" and r["judge_B"].get("verdict") == "correct"]
    regressions = [r for r in all_rows
                   if r["judge_A"].get("verdict") == "correct" and r["judge_B"].get("verdict") == "wrong"]
    print(f"\nJudge-based flips:")
    print(f"  ↑↑ A=wrong → B=correct (rescues):      {len(rescues)}")
    print(f"  ↓↓ A=correct → B=wrong (regressions):  {len(regressions)}")
    for r in rescues:
        print(f"\n  ↑↑ [{r['conv_id']} cat{r['category']}] fired={r['fired']} sessions={r['inject_sessions']}")
        print(f"    Q : {r['question']}")
        print(f"    GT: {r['ground_truth']}")
        print(f"    A : {r['answer_A'][:180]}")
        print(f"    B : {r['answer_B'][:180]}")
    for r in regressions:
        print(f"\n  ↓↓ [{r['conv_id']} cat{r['category']}] fired={r['fired']} sessions={r['inject_sessions']}")
        print(f"    Q : {r['question']}")
        print(f"    GT: {r['ground_truth']}")
        print(f"    A : {r['answer_A'][:180]}")
        print(f"    B : {r['answer_B'][:180]}")

    out = ROOT / os.environ.get("OUT", "ab_results.json")
    out.write_text(json.dumps({
        "config": {"gate_model": GATE_MODEL, "char_model": CHAR_MODEL, "judge_model": JUDGE_MODEL,
                   "window_size": WINDOW_SIZE, "sample_per_cat": SAMPLE_PER_CAT,
                   "convs": CONVS},
        "overall": overall_summary,
        "fire_rate": fires / total_qs if total_qs else 0,
        "cost": total_cost, "wall_time_s": total_time,
        "rows": all_rows,
    }, indent=2))
    print(f"\nResults saved → {out}")


if __name__ == "__main__":
    main()
