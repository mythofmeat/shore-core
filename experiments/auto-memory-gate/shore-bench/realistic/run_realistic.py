#!/usr/bin/env python3
"""
Realistic-mode driver. Sends natural conversational turns to the character
(Opus-4.7 chat model by default) and measures:

  - invoked_memory  : did the character call the memory tool?
  - tool_calls      : list of tools invoked
  - response        : captured character response
  - hallucinated    : judge verdict — did the character fabricate details?

The key metric is **invocation rate**. In the user's real Shore usage, Opus
rarely invokes memory (~3 calls per hundreds of messages), even with the
current strongly-worded tool description. This bench reproduces that failure
surface so we can measure changes against it.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
REPO = ROOT.parent.parent.parent
RESULTS = ROOT / "results"

REASONING = os.environ.get("BENCH_REASONING", "xhigh").lower()
TEMPLATES = ROOT / "templates" / (
    "realistic" if REASONING == "xhigh" else f"realistic-r-{REASONING}"
)

DAEMON_BIN = Path(os.environ.get("SHORE_DAEMON_BIN", REPO / "target/debug/shore-daemon"))
SHORE_BIN = Path(os.environ.get("SHORE_BIN", REPO / "target/debug/shore"))
JUDGE_SCRIPT = HERE / "judge_realistic.py"

CHAR_BY_CONV = {"conv-26": "Caroline", "conv-50": "Calvin"}


def _load_dotenv(path):
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
            q = v[0]
            end = v.find(q, 1)
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
    if _load_dotenv(_p):
        break


ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def strip_ansi(s: str) -> str:
    return ANSI_RE.sub("", s)


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


def extract_response(raw: str) -> str:
    """Grab the character's narrative text from `shore send` stdout.

    Shore CLI streams each tool/result framing as bracketed blocks:
      [tool: <name>] {json args}                     — single-line label
      [result: <multiline content, possibly many
      paragraphs and blank lines, ending with
      the closing bracket at end of line>]
      [<model> | in:N out:N ...]                     — single-line metrics footer

    The `[result: ...]` block spans multiple lines. We enter a "skip until
    closing bracket" state whenever a line opens such a block.
    """
    clean = strip_ansi(raw)
    out = []
    in_result = False
    for line in clean.split("\n"):
        l = line.strip()
        if in_result:
            # Result body — skip every line until one ends with `]`.
            if l.endswith("]"):
                in_result = False
            continue
        if not l:
            continue
        if l.startswith("[tool:"):
            # Tool-call label is single-line: "[tool: name] {json args}"
            continue
        if l.startswith("[result:"):
            # May be one line ([result: ...]) or multi-line.
            if l.endswith("]"):
                continue
            in_result = True
            continue
        # Metrics footer: "[model | in:N out:N ...]"
        if l.startswith("[") and "|" in l and "in:" in l:
            continue
        out.append(l)
    return "\n".join(out).strip()


def parse_daemon_log(daemon_log_path: Path) -> dict:
    """Scan daemon log for tool invocations."""
    if not daemon_log_path.exists():
        return {"invoked_memory": False, "tool_calls": []}
    text = strip_ansi(daemon_log_path.read_text(errors="replace"))
    # Memory agent starts via ask_memory_agent (the researcher layer).
    invoked_memory = "ask_memory_agent" in text or "Memory agent ask started" in text
    calls = []
    for name in (
        "memory", "check_time", "roll_dice", "web_search", "activity_heatmap",
        "set_next_wake", "scratchpad_list", "scratchpad_read",
        "scratchpad_write", "scratchpad_delete",
    ):
        # Count distinct tool_name invocations from tool_call records.
        pat = re.compile(rf"tool_call.*?name[\"']?\s*[:=]\s*[\"']({re.escape(name)})[\"']")
        hits = len(pat.findall(text))
        if hits:
            calls.append({"tool": name, "count": hits})
    # Fallback: look for ask_memory_agent explicitly (researcher's outer tool).
    if invoked_memory and not any(c["tool"] == "memory" for c in calls):
        calls.append({"tool": "memory", "count": text.count("ask_memory_agent")})
    return {"invoked_memory": invoked_memory, "tool_calls": calls}


def run_turn(conv_id: str, char: str, user_text: str, log_dir: Path) -> dict:
    start = time.time()
    tmp = Path(tempfile.mkdtemp(prefix=f"realistic-{conv_id}-"))
    try:
        src = TEMPLATES / conv_id
        for child in src.iterdir():
            if child.is_dir():
                shutil.copytree(child, tmp / child.name)
            else:
                shutil.copy(child, tmp / child.name)

        env = os.environ.copy()
        env["SHORE_CONFIG_DIR"] = str(tmp / "config")
        env["SHORE_DATA_DIR"] = str(tmp / "data")
        env["SHORE_RUNTIME_DIR"] = str(tmp / "runtime")

        instance_id = f"realistic-{os.getpid()}-{int(time.time()*1000)%100000}"
        daemon_log = log_dir / f"daemon-{instance_id}.log"
        daemon_log.parent.mkdir(parents=True, exist_ok=True)
        with open(daemon_log, "w") as log_f:
            daemon = subprocess.Popen(
                [str(DAEMON_BIN), "--instance-id", instance_id, "--addr", "127.0.0.1:0"],
                env=env, stdout=log_f, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
            )
        try:
            addr = wait_for_daemon(tmp / "runtime" / "instances.json", instance_id)
            if addr is None:
                return {"ok": False, "error": "daemon did not register"}
            try:
                result = subprocess.run(
                    [str(SHORE_BIN), "--addr", addr, "--character", char, "send", user_text],
                    env=env, capture_output=True, text=True, timeout=600,
                )
                raw = (result.stdout or "") + (result.stderr or "")
                returncode = result.returncode
            except subprocess.TimeoutExpired as te:
                raw = ((te.stdout or b"").decode(errors="replace")
                       + (te.stderr or b"").decode(errors="replace"))
                returncode = -1  # sentinel for timeout
                time.sleep(0.2)
                tool_info = parse_daemon_log(daemon_log)
                response = extract_response(raw)
                return {
                    "ok": False,
                    "error": f"CLI timeout after 600s (but daemon may have completed — check log)",
                    "returncode": returncode,
                    "raw": raw,
                    "response": response,
                    "invoked_memory": tool_info["invoked_memory"],
                    "tool_calls": tool_info["tool_calls"],
                    "elapsed_s": round(time.time() - start, 2),
                }
            # Give the daemon a moment to flush its log before we parse it.
            time.sleep(0.2)
            tool_info = parse_daemon_log(daemon_log)
            response = extract_response(raw)
            elapsed = time.time() - start
            return {
                "ok": result.returncode == 0,
                "returncode": result.returncode,
                "response": response,
                "invoked_memory": tool_info["invoked_memory"],
                "tool_calls": tool_info["tool_calls"],
                "raw": raw,
                "elapsed_s": round(elapsed, 2),
            }
        finally:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def judge_hallucination(user_text: str, memory_hook: str, response: str) -> dict:
    if not JUDGE_SCRIPT.exists():
        return {"verdict": "skipped"}
    try:
        r = subprocess.run(
            ["python3", str(JUDGE_SCRIPT)],
            input=json.dumps({
                "user_text": user_text,
                "memory_hook": memory_hook,
                "response": response,
            }),
            capture_output=True, text=True, timeout=60,
        )
        if r.returncode != 0:
            return {"verdict": "error", "reason": r.stderr[:200]}
        return json.loads(r.stdout.strip())
    except (subprocess.TimeoutExpired, json.JSONDecodeError) as e:
        return {"verdict": "error", "reason": str(e)[:200]}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--conv", default="conv-26,conv-50")
    ap.add_argument("--arm", default="opus-bare")
    ap.add_argument("--out", default=None)
    ap.add_argument("--skip-judge", action="store_true")
    ap.add_argument("--max-turns", type=int, default=0,
                    help="cap turns per conv (0 = all)")
    args = ap.parse_args()

    convs = args.conv.split(",")
    ts = time.strftime("%Y%m%d-%H%M%S")
    out_path = Path(args.out) if args.out else RESULTS / f"realistic-{args.arm}-{ts}.json"
    out_path.parent.mkdir(parents=True, exist_ok=True)
    log_dir = RESULTS / f"logs-realistic-{args.arm}-{ts}"

    rows = []
    invoked = 0
    total = 0

    for conv_id in convs:
        char = CHAR_BY_CONV.get(conv_id)
        if not char:
            continue
        turns_path = HERE / f"turns-{conv_id}.jsonl"
        if not turns_path.exists():
            print(f"skip: no turns file for {conv_id}")
            continue
        turns = [json.loads(l) for l in turns_path.read_text().splitlines() if l.strip()]
        if args.max_turns > 0:
            turns = turns[:args.max_turns]
        print(f"\n=== {conv_id} [{char}] — {len(turns)} turns ===")

        for i, t in enumerate(turns, 1):
            user_text = t["user_text"]
            hook = t.get("memory_hook", "")
            print(f"  [{i}/{len(turns)}] user: {user_text[:80]}")
            print(f"           hook: {hook[:80]}")
            result = run_turn(conv_id, char, user_text, log_dir)
            total += 1
            if not result.get("ok"):
                err = result.get("error") or f"rc={result.get('returncode')}"
                print(f"           ERROR: {err}")
                rows.append({"conv_id": conv_id, "turn_id": t["turn_id"],
                             "user_text": user_text, "memory_hook": hook,
                             "invoked_memory": result.get("invoked_memory", False),
                             "tool_calls": result.get("tool_calls", []),
                             "response": result.get("response", ""),
                             "raw": (result.get("raw") or "")[:4000],
                             "returncode": result.get("returncode"),
                             "verdict": "error"})
                continue
            invoked_mem = result["invoked_memory"]
            if invoked_mem:
                invoked += 1
            print(f"           invoked_memory: {invoked_mem}")
            print(f"           tools: {result.get('tool_calls') or '[]'}")
            print(f"           response: {result['response'][:160]}")

            if args.skip_judge:
                verdict = {"verdict": "skipped"}
            else:
                verdict = judge_hallucination(user_text, hook, result["response"])
            print(f"           judge: {verdict.get('verdict','?')}  ({verdict.get('reason','')[:80]})")

            rows.append({
                "conv_id": conv_id, "turn_id": t["turn_id"],
                "user_text": user_text, "memory_hook": hook,
                "invoked_memory": invoked_mem,
                "tool_calls": result.get("tool_calls", []),
                "response": result["response"],
                "verdict": verdict.get("verdict"),
                "verdict_reason": verdict.get("reason", ""),
                "elapsed_s": result.get("elapsed_s"),
                "raw": (result.get("raw") or "")[:4000],
            })

    print(f"\n{'='*70}")
    print(f"REALISTIC arm={args.arm}")
    print(f"{'='*70}")
    print(f"total turns        : {total}")
    print(f"invoked memory     : {invoked} ({invoked/total*100:.1f}%)" if total else "no turns")
    verdict_counts = {}
    for r in rows:
        verdict_counts[r.get("verdict")] = verdict_counts.get(r.get("verdict"), 0) + 1
    for v, c in verdict_counts.items():
        print(f"  judge={v:<12}: {c}")

    out_path.write_text(json.dumps({
        "arm": args.arm,
        "summary": {
            "total_turns": total,
            "invoked_memory": invoked,
            "invocation_rate": (invoked / total) if total else 0,
            "verdicts": verdict_counts,
        },
        "rows": rows,
    }, indent=2))
    print(f"\nResults → {out_path}")


if __name__ == "__main__":
    main()
