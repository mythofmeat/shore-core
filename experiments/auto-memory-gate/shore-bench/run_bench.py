#!/usr/bin/env python3
"""
Memory benchmark driver — runs against real Shore binaries.

For each sampled QA:
  1. Copy template profile to a fresh tmpdir.
  2. Spawn shore-daemon against the tmpdir.
  3. Send the question via `shore send`.
  4. Capture the character's response.
  5. Kill the daemon, clean up.
  6. Score with an external judge (judge.py, calls Haiku directly).

Flags:
  --conv conv-26[,conv-50]    Convs to benchmark (default: both)
  --n-per-cat 5               QA count per category per conv (0 = all)
  --seed 42                   Sampling seed
  --out results/run-<ts>.json Results file
  --categories 1,2,3,4        Which LoCoMo categories to include (default: all)

The benchmark measures Shore's current behavior. It makes NO assumptions about
Shore internals — it drives `shore send` via the CLI and reads the response.
Shore can be updated and the benchmark does not need to change, unless the CLI
contract breaks.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from collections import defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent.parent
FIXTURES = ROOT / "fixtures"
RESULTS = ROOT / "results"

PROFILE = os.environ.get("BENCH_MODEL_PROFILE", "haiku")
TEMPLATES = ROOT / "templates" / PROFILE


def load_dotenv_into_current(path: Path) -> int:
    """Load `KEY=VALUE` lines from `path` into os.environ without overriding.
    Strips inline comments (` #...` after the value) and surrounding quotes.
    """
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


# Load production shore .env so profile=prod can see OPENROUTER_SHORE_PRIMARY etc.
# without requiring the user to pre-export them. Order matches the live daemon's
# resolution: the systemd unit sets SHORE_CONFIG_DIR=~/Documents/qifei/config, so
# that .env is authoritative. Fallback to the default shore config dir.
_DOTENV_CANDIDATES = [
    Path.home() / "Documents" / "qifei" / "config" / ".env",
    Path.home() / ".config" / "shore" / ".env",
]
for _p in _DOTENV_CANDIDATES:
    if load_dotenv_into_current(_p):
        break

DAEMON_BIN = Path(os.environ.get("SHORE_DAEMON_BIN", REPO / "target/debug/shore-daemon"))
SHORE_BIN = Path(os.environ.get("SHORE_BIN", REPO / "target/debug/shore"))
JUDGE_SCRIPT = ROOT / "judge.py"

CHAR_BY_CONV = {
    "conv-26": "Caroline",
    "conv-50": "Calvin",
}


def stratified_sample(questions_path, per_cat, seed, categories):
    import random
    rng = random.Random(seed)
    buckets = defaultdict(list)
    with open(questions_path) as f:
        for line in f:
            qa = json.loads(line)
            if qa["category"] in categories:
                buckets[qa["category"]].append(qa)
    sampled = []
    for c in sorted(categories):
        items = buckets.get(c, [])
        rng.shuffle(items)
        if per_cat == 0:
            sampled.extend(items)
        else:
            sampled.extend(items[:per_cat])
    return sampled


def wait_for_daemon(instances_path: Path, instance_id: str, timeout=10.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if instances_path.exists():
            try:
                data = json.loads(instances_path.read_text())
                if isinstance(data, list):
                    for e in data:
                        if e.get("id") == instance_id and e.get("addr"):
                            return e["addr"]
            except (json.JSONDecodeError, OSError):
                pass
        time.sleep(0.1)
    return None


def strip_ansi(s: str) -> str:
    import re
    return re.sub(r"\x1b\[[0-9;]*m", "", s)


def extract_final_answer(raw_output: str) -> str:
    """Extract the character's final text answer from `shore send` stdout.

    `shore send` prints tool calls (prefixed `[tool: ...]`), tool results
    (prefixed `[result: ...]`), the final response, and a metrics footer
    (prefixed `[anthropic/... | in:N out:N ...]`).

    We want the last non-bracketed, non-empty line before the metrics footer.
    """
    clean = strip_ansi(raw_output).strip()
    lines = [l.strip() for l in clean.split("\n")]
    # Strip metrics footer and tool/result lines; keep narrative.
    text_lines = []
    for l in lines:
        if not l:
            continue
        if l.startswith("[tool:") or l.startswith("[result:"):
            continue
        if l.startswith("[") and ("|" in l and "in:" in l):
            continue
        text_lines.append(l)
    return "\n".join(text_lines).strip()


def run_one_question(conv_id: str, char_name: str, question: str, log_dir: Path) -> dict:
    """Spawn a fresh daemon, send the question, capture the response, kill, cleanup."""
    start = time.time()
    tmpdir = Path(tempfile.mkdtemp(prefix=f"shorebench-{conv_id}-"))
    try:
        # 1. Copy template
        src = TEMPLATES / conv_id
        for child in src.iterdir():
            if child.is_dir():
                shutil.copytree(child, tmpdir / child.name)
            else:
                shutil.copy(child, tmpdir / child.name)

        env = os.environ.copy()
        env["SHORE_CONFIG_DIR"] = str(tmpdir / "config")
        env["SHORE_DATA_DIR"] = str(tmpdir / "data")
        env["SHORE_RUNTIME_DIR"] = str(tmpdir / "runtime")

        # 2. Spawn daemon
        instance_id = f"bench-{os.getpid()}-{int(time.time()*1000)%100000}"
        daemon_log = log_dir / f"daemon-{instance_id}.log"
        daemon_log.parent.mkdir(parents=True, exist_ok=True)
        with open(daemon_log, "w") as log_f:
            daemon = subprocess.Popen(
                [str(DAEMON_BIN), "--instance-id", instance_id, "--addr", "127.0.0.1:0"],
                env=env, stdout=log_f, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
            )
        try:
            addr = wait_for_daemon(tmpdir / "runtime" / "instances.json", instance_id)
            if addr is None:
                return {"ok": False, "error": "daemon did not register", "raw": ""}

            # 3. Send. Upstream OpenRouter rate limits on cheap models can
            # push a single question past the default 180s (e.g. Gemma-4
            # retry storms on inner agent). Bump to 600s and catch the
            # TimeoutExpired so one slow Q doesn't abort the whole run.
            try:
                result = subprocess.run(
                    [str(SHORE_BIN), "--addr", addr, "--character", char_name, "send", question],
                    env=env, capture_output=True, text=True, timeout=600,
                )
                raw = (result.stdout or "") + (result.stderr or "")
                answer = extract_final_answer(raw)
                elapsed = time.time() - start
                return {
                    "ok": result.returncode == 0,
                    "returncode": result.returncode,
                    "raw": raw,
                    "answer": answer,
                    "elapsed_s": round(elapsed, 2),
                }
            except subprocess.TimeoutExpired as e:
                raw = (e.stdout.decode("utf-8", "replace") if e.stdout else "") + \
                      (e.stderr.decode("utf-8", "replace") if e.stderr else "")
                return {
                    "ok": False,
                    "returncode": -1,
                    "error": "shore send timed out after 600s",
                    "raw": raw,
                    "answer": "",
                    "elapsed_s": round(time.time() - start, 2),
                }
        finally:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
    finally:
        try:
            shutil.rmtree(tmpdir, ignore_errors=True)
        except Exception:
            pass


def judge_answer(question: str, ground_truth: str, answer: str) -> dict:
    """Call judge.py as a subprocess; returns {verdict, reason}."""
    if not JUDGE_SCRIPT.exists():
        return {"verdict": "skipped", "reason": "judge.py not found"}
    try:
        r = subprocess.run(
            ["python3", str(JUDGE_SCRIPT)],
            input=json.dumps({"question": question, "ground_truth": ground_truth, "answer": answer}),
            capture_output=True, text=True, timeout=60,
        )
        if r.returncode != 0:
            return {"verdict": "error", "reason": r.stderr[:200]}
        return json.loads(r.stdout.strip())
    except (subprocess.TimeoutExpired, json.JSONDecodeError) as e:
        return {"verdict": "error", "reason": str(e)[:200]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--conv", default="conv-26,conv-50",
                    help="comma-separated conv_ids")
    ap.add_argument("--n-per-cat", type=int, default=5,
                    help="QA per category per conv (0 = all)")
    ap.add_argument("--categories", default="1,2,3,4",
                    help="comma-separated LoCoMo categories to include")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--out", default=None)
    ap.add_argument("--arm", default="bare",
                    help="label for this run (appears in results)")
    args = ap.parse_args()

    convs = args.conv.split(",")
    categories = [int(c) for c in args.categories.split(",") if c]
    ts = time.strftime("%Y%m%d-%H%M%S")
    arm_label = f"{args.arm}-{PROFILE}"
    out_path = Path(args.out) if args.out else RESULTS / f"{arm_label}-{ts}.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    log_dir = RESULTS / f"logs-{arm_label}-{ts}"

    if not TEMPLATES.exists():
        print(f"ERROR: template dir {TEMPLATES} not found. "
              f"Run `BENCH_MODEL_PROFILE={PROFILE} prepare_template.py` first.", file=sys.stderr)
        sys.exit(1)
    print(f"profile={PROFILE}  templates={TEMPLATES}")

    rows = []
    verdict_counts = {"correct": 0, "partial": 0, "wrong": 0, "error": 0, "skipped": 0}
    by_cat = defaultdict(lambda: {"correct": 0, "partial": 0, "wrong": 0, "error": 0, "n": 0})

    for conv_id in convs:
        char_name = CHAR_BY_CONV.get(conv_id)
        if not char_name:
            print(f"skip: no character for {conv_id}", file=sys.stderr)
            continue
        qs_path = FIXTURES / conv_id / "questions.jsonl"
        qas = stratified_sample(qs_path, args.n_per_cat, args.seed, categories)
        print(f"\n=== {conv_id} [{char_name}] — {len(qas)} questions ===")

        for i, qa in enumerate(qas, 1):
            q = qa["question"]
            gt = qa["ground_truth"]
            cat = qa["category"]
            print(f"  [{i:2d}/{len(qas)}] c{cat} Q: {q[:70]}")
            result = run_one_question(conv_id, char_name, q, log_dir)
            if not result["ok"] and "error" in result:
                print(f"         ERROR: {result['error']}")
            answer = result.get("answer", "")
            print(f"         GT: {gt}")
            print(f"         A : {answer[:140]}")

            verdict = judge_answer(q, gt, answer) if result["ok"] else {"verdict": "error", "reason": result.get("error", "")}
            v = verdict.get("verdict", "error")
            verdict_counts[v] = verdict_counts.get(v, 0) + 1
            by_cat[cat]["n"] += 1
            by_cat[cat][v] = by_cat[cat].get(v, 0) + 1
            print(f"         verdict: {v}  ({verdict.get('reason','')[:80]})")

            rows.append({
                "conv_id": conv_id, "category": cat, "question": q, "ground_truth": gt,
                "answer": answer, "elapsed_s": result.get("elapsed_s"),
                "returncode": result.get("returncode"),
                "raw": result.get("raw", "")[:4000],
                "verdict": v, "verdict_reason": verdict.get("reason", ""),
            })

    # Report
    total = sum(verdict_counts.values())
    print(f"\n{'='*80}")
    print(f"RESULTS — arm={arm_label}")
    print(f"{'='*80}")
    print(f"total questions: {total}")
    for v in ("correct", "partial", "wrong", "error"):
        c = verdict_counts.get(v, 0)
        pct = c / total if total else 0
        print(f"  {v:<9}: {c:>4} ({pct:>6.1%})")
    print()
    print(f"{'Category':<14} {'correct':>8} {'partial':>8} {'wrong':>8} {'n':>4}")
    cat_names = {1: "multi-hop", 2: "temporal", 3: "open-domain", 4: "single-hop"}
    for cat in sorted(by_cat.keys()):
        b = by_cat[cat]
        n = b["n"]
        print(f"  {cat}. {cat_names.get(cat,'?'):<10} "
              f"{b.get('correct',0):>7} ({b.get('correct',0)/n:>5.0%}) "
              f"{b.get('partial',0):>7} ({b.get('partial',0)/n:>5.0%}) "
              f"{b.get('wrong',0):>7} ({b.get('wrong',0)/n:>5.0%}) "
              f"{n:>4}")

    out_path.write_text(json.dumps({
        "arm": arm_label,
        "config": {
            "profile": PROFILE,
            "convs": convs, "n_per_cat": args.n_per_cat, "categories": categories,
            "seed": args.seed,
            "daemon_bin": str(DAEMON_BIN), "shore_bin": str(SHORE_BIN),
        },
        "summary": {
            "total": total,
            "verdicts": verdict_counts,
            "by_category": {str(c): dict(v) for c, v in by_cat.items()},
        },
        "rows": rows,
    }, indent=2))
    print(f"\nResults saved → {out_path}")


if __name__ == "__main__":
    main()
