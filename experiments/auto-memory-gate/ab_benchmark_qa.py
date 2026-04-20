#!/usr/bin/env python3
"""
Phase 2.5 — Question-asker gate + filter + retrieval.

This benchmark validates the ORIGINALLY INTENDED gate architecture:
  1. Classifier: fire / no-fire on this turn
  2. Question-asker: if firing, formulate recall questions about the
     conversation
  3. Retriever: for each question, FTS-search the memory store
     (here: LoCoMo observation facts, which match Shore's Entry shape)
  4. Filter: receive retrieved entries, decide worth-injecting, synthesize
     short injection with citations

Arm A (no memory) — same as ab_benchmark.py v3.
Arm B (question-asker gate) — replaces the v7 selector.

Scoring: token F1, recall, substring, judge=correct, judge≥partial.
"""

import json
import os
import re
import sqlite3
import sys
import time
from collections import Counter, defaultdict
from pathlib import Path
from urllib import request, error

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent
DATASET = REPO / "shore-daemon/tests/data/locomo10.json"

WINDOW_SIZE = int(os.environ.get("WINDOW_SIZE", "12"))
SAMPLE_PER_CAT = int(os.environ.get("SAMPLE_PER_CAT", "5"))
CONVS = os.environ.get("CONVS", "conv-26,conv-50").split(",")
SEED = int(os.environ.get("SEED", "42"))
RETRIEVAL_K = int(os.environ.get("RETRIEVAL_K", "6"))  # top-k per question

GATE_MODEL = os.environ.get("GATE_MODEL", "google/gemma-4-31b-it")
FILTER_MODEL = os.environ.get("FILTER_MODEL", "google/gemma-4-31b-it")
CHAR_MODEL = os.environ.get("CHAR_MODEL", "anthropic/claude-haiku-4-5")
JUDGE_MODEL = os.environ.get("JUDGE_MODEL", "google/gemma-4-31b-it")

API_KEY = os.environ.get("OPENROUTER_SHORE_TEST")
if not API_KEY:
    print("ERROR: OPENROUTER_SHORE_TEST not set", file=sys.stderr)
    sys.exit(1)


# ── scoring (copied from ab_benchmark.py) ──────────────────────────────────


def tokenize(s):
    return [t for t in re.split(r"[^a-z0-9]+", s.lower()) if t]


def token_f1(pred, gt):
    p, g = tokenize(pred), tokenize(gt)
    if not g and not p: return 1.0
    if not g or not p: return 0.0
    remain = Counter(g)
    matches = 0
    for t in p:
        if remain[t] > 0:
            remain[t] -= 1
            matches += 1
    if matches == 0: return 0.0
    precision = matches / len(p)
    recall = matches / len(g)
    return 2 * precision * recall / (precision + recall)


def score_qa(pred, qa):
    a = qa.get("answer")
    if a is None: return 0.0
    cat = qa["category"]
    if cat == 1:
        subs = [s.strip() for s in str(a).split(",") if s.strip()]
        if not subs: return 0.0
        return sum(token_f1(pred, s) for s in subs) / len(subs)
    if cat == 3:
        first = str(a).split(";")[0].strip()
        return token_f1(pred, first)
    if cat in (2, 4): return token_f1(pred, str(a))
    return 0.0


def recall_score(pred, gt):
    p, g = tokenize(pred), tokenize(gt)
    if not g: return 1.0 if not p else 0.0
    pset = Counter(p)
    matches = 0
    for t in g:
        if pset[t] > 0:
            pset[t] -= 1
            matches += 1
    return matches / len(g)


def recall_score_qa(pred, qa):
    a = qa.get("answer")
    if a is None: return 0.0
    cat = qa["category"]
    if cat == 1:
        subs = [s.strip() for s in str(a).split(",") if s.strip()]
        if not subs: return 0.0
        return sum(recall_score(pred, s) for s in subs) / len(subs)
    if cat == 3:
        first = str(a).split(";")[0].strip()
        return recall_score(pred, first)
    return recall_score(pred, str(a))


def substring_score(pred, gt):
    def norm(s): return re.sub(r"[^a-z0-9\s]", "", s.lower()).strip()
    p, g = norm(pred), norm(gt)
    if not g: return 1.0 if not p else 0.0
    return 1.0 if g in p else 0.0


def substring_score_qa(pred, qa):
    a = qa.get("answer")
    if a is None: return 0.0
    cat = qa["category"]
    if cat == 1:
        subs = [s.strip() for s in str(a).split(",") if s.strip()]
        if not subs: return 0.0
        return sum(substring_score(pred, s) for s in subs) / len(subs)
    if cat == 3:
        first = str(a).split(";")[0].strip()
        return substring_score(pred, first)
    return substring_score(pred, str(a))


# ── LoCoMo loading ─────────────────────────────────────────────────────────


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
    # Also track session date for each session
    sess_dates = {}
    for k, v in conv["conversation"].items():
        if k.endswith("_date_time"):
            try:
                sid = int(k.split("_")[1])
                sess_dates[sid] = v
            except ValueError: pass
    for k in skeys:
        sid = int(k.split("_")[1])
        for t in conv["conversation"][k]:
            flat.append({
                "session": sid,
                "speaker": t.get("speaker"),
                "dia_id": t.get("dia_id"),
                "text": t.get("text", ""),
            })

    # Observation facts → entries (match Shore Entry shape at a high level)
    entries = []
    for obs_key, speaker_dict in conv.get("observation", {}).items():
        if not obs_key.endswith("_observation"): continue
        try:
            sid = int(obs_key.split("_")[1])
        except ValueError: continue
        for speaker, items in speaker_dict.items():
            for item in items:
                if not isinstance(item, (list, tuple)) or len(item) < 2:
                    continue
                text = item[0]
                dia_id = item[1]
                if not text: continue
                entries.append({
                    "id": len(entries),
                    "session": sid,
                    "speaker": speaker,
                    "dia_id": dia_id,
                    "text": text,
                    "session_date": sess_dates.get(sid, ""),
                })

    speakers = [conv["conversation"].get("speaker_a"), conv["conversation"].get("speaker_b")]
    return flat, entries, [s for s in speakers if s]


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


# ── SQLite FTS5 retriever ──────────────────────────────────────────────────


def build_fts(entries):
    """Build an in-memory FTS5 index over entries. Returns the connection."""
    conn = sqlite3.connect(":memory:")
    conn.execute("CREATE VIRTUAL TABLE entries USING fts5(id UNINDEXED, session UNINDEXED, speaker UNINDEXED, dia_id UNINDEXED, text, session_date UNINDEXED)")
    conn.executemany(
        "INSERT INTO entries (id, session, speaker, dia_id, text, session_date) VALUES (?, ?, ?, ?, ?, ?)",
        [(e["id"], e["session"], e["speaker"], e["dia_id"], e["text"], e["session_date"]) for e in entries],
    )
    return conn


def fts_query(conn, q, k):
    # FTS5 needs sanitized input; simplify to alphanumeric token OR query
    toks = [t for t in re.split(r"[^a-zA-Z0-9]+", q) if t]
    if not toks:
        return []
    match = " OR ".join(toks)
    try:
        rows = conn.execute(
            "SELECT id, session, speaker, dia_id, text, session_date, rank FROM entries WHERE entries MATCH ? ORDER BY rank LIMIT ?",
            (match, k),
        ).fetchall()
    except sqlite3.OperationalError:
        return []
    return [{"id": r[0], "session": r[1], "speaker": r[2], "dia_id": r[3],
             "text": r[4], "session_date": r[5]} for r in rows]


# ── LLM plumbing ────────────────────────────────────────────────────────────


def call_openrouter(model, messages, response_format=None, temperature=1.0, extra=None, max_retries=2):
    payload = {"model": model, "messages": messages, "temperature": temperature, "usage": {"include": True}}
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
            return {"text": text, "prompt_tokens": usage.get("prompt_tokens", 0),
                    "completion_tokens": usage.get("completion_tokens", 0),
                    "cost": float(usage.get("cost", 0) or 0), "elapsed": elapsed}
        except error.HTTPError as e:
            last_err = f"HTTP {e.code}: {e.read().decode()[:200]}"
        except Exception as e:
            last_err = f"{type(e).__name__}: {e}"
    return {"error": last_err, "elapsed": 0.0}


# ── Gate v8 (question-asker) ───────────────────────────────────────────────


GATE_V8_SYSTEM = """You are a memory gate for a conversational character.

You see (a) a rolling conversation window and (b) a new incoming message the
character is about to respond to. You do NOT see memory content yet.

Your job: decide whether the character's response would benefit from recalling
long-term memory. If yes, formulate 1-3 SPECIFIC questions that the memory
system should try to answer. Each question must be answerable from prior
conversation facts (dates, events, statements, decisions, people, objects).

Fire when the incoming message:
- References a past event/object/statement ("last year", "remember when",
  "that book you recommended")
- Asks a question whose answer requires prior conversation content
- Continues a multi-session arc with reflection or milestone language
- Uses oblique shared-history phrasing ("our gang", "back then")
- Is an explicit catch-up opener ("long time no see, what's new?")

Do NOT fire when:
- The message is conversational filler (cool, thanks, totally)
- It's a direct reply to something in the window
- It's a fresh event being narrated with no callbacks
- It's a supportive declarative statement with no question

Questions must be specific and memory-answerable. Good:
  "Which artists were mentioned as collaborators?"
  "When did Caroline first mention adoption?"
  "What object did Calvin's grandmother give him?"
Bad:
  "What is the general vibe of their friendship?" (not memory-answerable)
  "How does Caroline feel?" (too vague)

Output STRICT JSON only:
{"fire": true|false, "reason": "one sentence", "questions": ["q1", "q2", ...]}

If fire=false, questions must be an empty array."""


def run_gate_v8(window_text, incoming):
    user = (
        f"# Recent window (last {WINDOW_SIZE} turns)\n{window_text}\n\n"
        f"# Incoming message\n{incoming}\n\n"
        f"Decide and output JSON."
    )
    r = call_openrouter(
        GATE_MODEL,
        [{"role": "system", "content": GATE_V8_SYSTEM}, {"role": "user", "content": user}],
        response_format={"type": "json_object"},
        temperature=1.0,
    )
    if "error" in r:
        return {"fire": False, "reason": f"gate error: {r['error']}", "questions": []}, r
    try:
        parsed = json.loads(r["text"].strip())
    except Exception as e:
        return {"fire": False, "reason": f"gate parse error: {e}", "questions": []}, r
    if not parsed.get("fire"):
        parsed["questions"] = []
    else:
        parsed["questions"] = [q for q in (parsed.get("questions") or []) if isinstance(q, str) and q.strip()][:3]
        if not parsed["questions"]:
            parsed["fire"] = False
    return parsed, r


# ── Filter v8 (decide worth-injecting + synthesize) ─────────────────────────


FILTER_V8_SYSTEM = """You are the second stage of a memory gate. You see:
1. The recent conversation window
2. The new incoming message the character is about to respond to
3. The questions a prior stage asked of long-term memory
4. The entries that memory retrieval returned, each with its own ID

Your job: decide whether the retrieved entries are worth injecting into the
character's context. If yes, synthesize a SHORT prose summary (under 80 words)
that is STRICTLY grounded in the retrieved entries, and cite the entry IDs you
used.

CRITICAL rules:
- Every factual claim in your injection MUST be traceable to at least one
  cited entry. If an entry doesn't say something, you don't either.
- Do NOT invent emotional framing, dates, or conclusions that aren't in the
  entries.
- If none of the retrieved entries actually answer the questions or help with
  the incoming message, output worth_injecting=false. Do NOT fire just because
  entries were retrieved — retrieval can miss.
- Keep the injection tight. Relevant facts only. No preamble.

Output STRICT JSON only:
{"worth_injecting": true|false, "injection": "short prose or null", "cited_entry_ids": [int, ...], "pointers": [str, ...]}

If worth_injecting=false: injection=null, cited_entry_ids=[], pointers may be
empty or contain topic titles the character could search further.
If worth_injecting=true: cited_entry_ids must be a non-empty subset of the
provided entry IDs, and every fact in the injection must come from a cited
entry."""


def run_filter_v8(window_text, incoming, questions, retrieved):
    entries_block = "\n".join(
        f"[entry {e['id']}] (session {e['session']}, {e['speaker']}, {e['session_date']}): {e['text']}"
        for e in retrieved
    ) or "(no entries retrieved)"
    qs_block = "\n".join(f"- {q}" for q in questions) or "(none)"
    user = (
        f"# Recent window (last {WINDOW_SIZE} turns)\n{window_text}\n\n"
        f"# Incoming message\n{incoming}\n\n"
        f"# Memory questions asked\n{qs_block}\n\n"
        f"# Retrieved entries\n{entries_block}\n\n"
        f"Decide and output JSON."
    )
    r = call_openrouter(
        FILTER_MODEL,
        [{"role": "system", "content": FILTER_V8_SYSTEM}, {"role": "user", "content": user}],
        response_format={"type": "json_object"},
        temperature=0.5,
    )
    if "error" in r:
        return {"worth_injecting": False, "injection": None,
                "cited_entry_ids": [], "pointers": [],
                "error": r.get("error", "")}, r
    try:
        parsed = json.loads(r["text"].strip())
    except Exception as e:
        return {"worth_injecting": False, "injection": None,
                "cited_entry_ids": [], "pointers": [],
                "error": f"parse: {e}"}, r
    # Validate citations
    valid_ids = {e["id"] for e in retrieved}
    cited = parsed.get("cited_entry_ids") or []
    try:
        cited_valid = {int(c) for c in cited if int(c) in valid_ids}
    except (TypeError, ValueError):
        cited_valid = set()
    if parsed.get("worth_injecting") and not cited_valid:
        parsed["worth_injecting"] = False
        parsed["injection"] = None
        parsed["_citation_invalid"] = True
    parsed["cited_entry_ids"] = sorted(cited_valid)
    return parsed, r


# ── character answerer ──────────────────────────────────────────────────────


def build_char_prompt(responder, window_text, injection, question, other):
    sys = (
        "You are answering a factual recall question about a long-running conversation. "
        "Respond with the SHORTEST POSSIBLE answer — usually 1 to 5 words. "
        "Match the format of the question:\n"
        "  - 'when' → a date or year\n"
        "  - 'what did X do' → a verb phrase or short noun phrase\n"
        "  - 'what items/activities/events' → a comma-separated list, no surrounding prose\n"
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


# ── judge ────────────────────────────────────────────────────────────────────


JUDGE_SYSTEM = """You are evaluating whether a character's answer captures the facts in a ground-truth answer to a factual recall question.

Classify the answer as:
- "correct": contains all essential facts from the ground truth, even if phrased differently
- "partial": contains SOME but not all essential facts
- "wrong": does not capture the essential facts (including "not available" refusals unless ground truth is explicitly unanswerable)

Respond with STRICT JSON only:
{"verdict": "correct"|"partial"|"wrong", "reason": "one short sentence"}"""


def judge_answer(question, ground_truth, answer):
    user = f"Question: {question}\nGround truth: {ground_truth}\nCharacter's answer: {answer}\n"
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


# ── main ────────────────────────────────────────────────────────────────────


METRICS = ("f1", "recall", "substring", "judge_correct", "judge_any")


def format_window(turns):
    return "\n".join(f"{t['speaker']}: {t['text']}" for t in turns)


def main():
    scores = {arm: {m: defaultdict(list) for m in METRICS} for arm in ("A", "B")}
    fires = 0
    injects = 0
    total_qs = 0
    total_cost = 0.0
    total_time = 0.0
    all_rows = []

    for conv_id in CONVS:
        print(f"\n{'='*100}\nCONV: {conv_id}\n{'='*100}")
        flat, entries, speakers = load_conv(conv_id)
        data = json.loads(DATASET.read_text())
        conv = next(c for c in data if c["sample_id"] == conv_id)
        qas = stratified_sample(conv["qa"], SAMPLE_PER_CAT, SEED)
        fts = build_fts(entries)
        print(f"  entries: {len(entries)}")

        pos = len(flat)
        window = flat[max(0, pos - WINDOW_SIZE):pos]
        window_text = format_window(window)
        responder = speakers[0] if speakers else "the character"
        other = speakers[1] if len(speakers) > 1 else "the user"

        for i, qa in enumerate(qas):
            q = qa["question"]
            gt = str(qa.get("answer", ""))
            cat = qa["category"]
            total_qs += 1

            # Stage 1: gate v8 (fire + questions)
            gate_res, gate_meta = run_gate_v8(window_text, q)
            if "cost" in gate_meta:
                total_cost += gate_meta["cost"]; total_time += gate_meta["elapsed"]
            fired = bool(gate_res.get("fire"))
            fires += int(fired)

            # Stage 2: retrieve (only if fired)
            retrieved = []
            if fired:
                seen_ids = set()
                for mq in gate_res["questions"]:
                    for row in fts_query(fts, mq, RETRIEVAL_K):
                        if row["id"] not in seen_ids:
                            seen_ids.add(row["id"])
                            retrieved.append(row)
                # Cap total retrieval size
                retrieved = retrieved[:12]

            # Stage 3: filter + synthesize
            filter_res = {"worth_injecting": False, "injection": None,
                          "cited_entry_ids": [], "pointers": []}
            if fired and retrieved:
                filter_res, filter_meta = run_filter_v8(
                    window_text, q, gate_res["questions"], retrieved
                )
                if "cost" in filter_meta:
                    total_cost += filter_meta["cost"]; total_time += filter_meta["elapsed"]

            injected = bool(filter_res.get("worth_injecting"))
            injects += int(injected)
            injection = filter_res.get("injection") or ""

            # Arm A: no memory
            ans_A, meta_A = answer_question(responder, window_text, "", q, other)
            # Arm B: question-asker pipeline result
            ans_B, meta_B = answer_question(
                responder, window_text, injection if injected else "", q, other
            )
            for m in (meta_A, meta_B):
                if "cost" in m:
                    total_cost += m["cost"]; total_time += m["elapsed"]

            # Judge both
            judge_A_res, judge_A_meta = judge_answer(q, gt, ans_A)
            judge_B_res, judge_B_meta = judge_answer(q, gt, ans_B)
            for m in (judge_A_meta, judge_B_meta):
                if "cost" in m:
                    total_cost += m["cost"]; total_time += m["elapsed"]

            def jv(res):
                v = res.get("verdict", "wrong")
                return float(v == "correct"), float(v in ("correct", "partial"))
            j_corr_A, j_any_A = jv(judge_A_res)
            j_corr_B, j_any_B = jv(judge_B_res)

            row_scores = {
                "A": {"f1": score_qa(ans_A, qa), "recall": recall_score_qa(ans_A, qa),
                      "substring": substring_score_qa(ans_A, qa),
                      "judge_correct": j_corr_A, "judge_any": j_any_A},
                "B": {"f1": score_qa(ans_B, qa), "recall": recall_score_qa(ans_B, qa),
                      "substring": substring_score_qa(ans_B, qa),
                      "judge_correct": j_corr_B, "judge_any": j_any_B},
            }
            for arm in ("A", "B"):
                for m in METRICS:
                    scores[arm][m][cat].append(row_scores[arm][m])

            tag_fire = f"🔥{len(gate_res.get('questions',[]))}q" if fired else "  "
            tag_inject = f"💉{len(filter_res.get('cited_entry_ids',[]))}" if injected else "  "
            jmark = f"{judge_A_res.get('verdict','?')[:4]:4s}→{judge_B_res.get('verdict','?')[:4]:4s}"
            print(f"  [c{cat}] {tag_fire} {tag_inject} {jmark}  f1 {row_scores['A']['f1']:.2f}→{row_scores['B']['f1']:.2f} | {q[:60]}")

            all_rows.append({
                "conv_id": conv_id, "category": cat, "question": q, "ground_truth": gt,
                "fired": fired, "questions": gate_res.get("questions", []),
                "retrieved_count": len(retrieved), "injected": injected,
                "cited_entry_ids": filter_res.get("cited_entry_ids", []),
                "gate_reason": gate_res.get("reason", ""),
                "filter_reason": filter_res.get("reason", ""),
                "injection": filter_res.get("injection", ""),
                "answer_A": ans_A, "answer_B": ans_B,
                "judge_A": judge_A_res, "judge_B": judge_B_res,
                "scores": row_scores,
            })

    # Report
    print("\n" + "=" * 100)
    print("AGGREGATE — Arm A (no memory) vs. Arm B (question-asker gate)")
    print("=" * 100)
    metric_labels = {"f1": "token F1", "recall": "recall", "substring": "substring",
                     "judge_correct": "judge=correct", "judge_any": "judge≥partial"}
    print(f"\n{'Metric':<15} {'Arm A':>10} {'Arm B':>10} {'Δ abs':>10} {'Δ rel':>10}")
    print("-" * 60)
    overall_summary = {}
    for m in METRICS:
        a_vals = [x for v in scores["A"][m].values() for x in v]
        b_vals = [x for v in scores["B"][m].values() for x in v]
        avg_a = sum(a_vals) / len(a_vals) if a_vals else 0
        avg_b = sum(b_vals) / len(b_vals) if b_vals else 0
        delta = avg_b - avg_a
        rel = f"{avg_b/avg_a:.2f}×" if avg_a > 0 else "—"
        overall_summary[m] = {"A": avg_a, "B": avg_b, "delta": delta}
        print(f"{metric_labels[m]:<15} {avg_a:>9.2%} {avg_b:>9.2%} {delta:>+9.2%} {rel:>10}")

    cat_names = {1: "multi-hop", 2: "temporal", 3: "open-domain", 4: "single-hop"}
    print(f"\nJudge (correct) by category:")
    print(f"{'Category':<14} {'A':>8} {'B':>8} {'Δ':>8} {'n':>4}")
    for cat in (1, 2, 3, 4):
        a = scores["A"]["judge_correct"].get(cat, [])
        b = scores["B"]["judge_correct"].get(cat, [])
        if not a: continue
        avg_a = sum(a) / len(a)
        avg_b = sum(b) / len(b)
        print(f"  {cat}. {cat_names[cat]:<10} {avg_a:>7.2%} {avg_b:>7.2%} {avg_b - avg_a:>+7.2%} {len(a):>4}")

    print(f"\nFire rate     : {fires}/{total_qs} = {fires/total_qs:.1%}")
    print(f"Inject rate   : {injects}/{total_qs} = {injects/total_qs:.1%}")
    print(f"Filter reject rate (fired but not injected): {(fires - injects)}/{fires if fires else 1} = {(fires - injects)/max(fires,1):.1%}")
    print(f"Total cost    : ${total_cost:.4f}")
    print(f"Wall time     : {total_time:.1f}s")

    # Judge flips
    rescues = [r for r in all_rows if r["judge_A"].get("verdict") == "wrong" and r["judge_B"].get("verdict") == "correct"]
    regressions = [r for r in all_rows if r["judge_A"].get("verdict") == "correct" and r["judge_B"].get("verdict") == "wrong"]
    print(f"\nJudge flips:")
    print(f"  ↑↑ rescues:      {len(rescues)}")
    print(f"  ↓↓ regressions:  {len(regressions)}")
    for r in rescues[:12]:
        print(f"\n  ↑↑ [{r['conv_id']} c{r['category']}] fired={r['fired']} injected={r['injected']} (cited {r['cited_entry_ids']})")
        print(f"    Q : {r['question']}")
        print(f"    GT: {r['ground_truth']}")
        print(f"    A : {r['answer_A'][:160]}")
        print(f"    B : {r['answer_B'][:160]}")
    for r in regressions:
        print(f"\n  ↓↓ [{r['conv_id']} c{r['category']}] fired={r['fired']} injected={r['injected']}")
        print(f"    Q : {r['question']}")
        print(f"    GT: {r['ground_truth']}")
        print(f"    A : {r['answer_A'][:160]}")
        print(f"    B : {r['answer_B'][:160]}")

    out = ROOT / os.environ.get("OUT", "ab_qa_results.json")
    out.write_text(json.dumps({
        "config": {"gate_model": GATE_MODEL, "filter_model": FILTER_MODEL,
                   "char_model": CHAR_MODEL, "judge_model": JUDGE_MODEL,
                   "window_size": WINDOW_SIZE, "sample_per_cat": SAMPLE_PER_CAT,
                   "convs": CONVS, "retrieval_k": RETRIEVAL_K},
        "overall": overall_summary,
        "fire_rate": fires / total_qs if total_qs else 0,
        "inject_rate": injects / total_qs if total_qs else 0,
        "cost": total_cost, "wall_time_s": total_time,
        "rows": all_rows,
    }, indent=2))
    print(f"\nResults saved → {out}")


if __name__ == "__main__":
    main()
