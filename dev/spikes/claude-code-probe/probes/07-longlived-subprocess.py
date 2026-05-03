#!/usr/bin/env python3
"""Probe 7: does `claude -p --input-format stream-json` linger between
user frames, or does it batch-then-exit?

We open the subprocess with pipes, write a single user frame, read
events until we see a `result` event, then write a SECOND user frame
without closing stdin. If the CLI lingers, we get a second response
referencing context from the first. If the CLI batched stdin and
exited after the first result, the second write fails or hangs.

Pass criteria:
- subprocess.poll() is None after the first `result` (still running)
- second user frame produces a second `result`
- second response references PURPLE (from the first frame's context)
- exit clean after stdin close
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

SPIKE_DIR = Path(__file__).resolve().parents[1]
RESULTS_DIR = SPIKE_DIR / "results"
RESULTS_DIR.mkdir(exist_ok=True)
LOG = RESULTS_DIR / "07-longlived.log"


def log(msg: str) -> None:
    line = f"[{time.time():.3f}] {msg}"
    print(line)
    with open(LOG, "a") as f:
        f.write(line + "\n")


def read_until_result(proc: subprocess.Popen, timeout_s: float = 60.0) -> dict | None:
    """Read stdout lines until we see a `result` event. Returns the result event."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        line = proc.stdout.readline()
        if not line:
            log("  stdout EOF before result event")
            return None
        line = line.strip()
        if not line:
            continue
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            log(f"  non-JSON line: {line[:200]}")
            continue
        t = ev.get("type")
        if t == "assistant":
            for b in ev.get("message", {}).get("content", []):
                if b.get("type") == "text":
                    log(f"  A.text: {b.get('text','')[:200]!r}")
        elif t == "result":
            log(f"  RESULT: subtype={ev.get('subtype')} is_error={ev.get('is_error')} num_turns={ev.get('num_turns')}")
            log(f"          result text: {(ev.get('result') or '')[:200]!r}")
            return ev
        elif t == "system":
            log(f"  SYSTEM init: model={ev.get('model')} session_id={ev.get('session_id')}")
        # other events ignored
    log("  TIMEOUT waiting for result")
    return None


def main() -> int:
    LOG.unlink(missing_ok=True)
    log("starting subprocess")

    cmd = [
        "claude",
        "--print",
        "--output-format", "stream-json",
        "--input-format", "stream-json",
        "--verbose",
        "--no-session-persistence",
        "--strict-mcp-config",
        "--model", "claude-haiku-4-5",
        "--tools", "",
        "--system-prompt", "You are a test fixture. Be terse.",
    ]

    proc = subprocess.Popen(
        cmd,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,  # line-buffered
    )

    try:
        # Frame 1
        log("=== writing frame 1 ===")
        frame1 = {"type":"user","message":{"role":"user","content":"Remember the secret word PURPLE for later. Just acknowledge."}}
        proc.stdin.write(json.dumps(frame1) + "\n")
        proc.stdin.flush()

        log("reading until first result")
        r1 = read_until_result(proc)
        if r1 is None:
            log("FAIL: no first result")
            return 1

        # Critical test: is the subprocess still alive?
        time.sleep(0.5)
        alive_after_1 = proc.poll() is None
        log(f"subprocess alive after first result? {alive_after_1}")

        if not alive_after_1:
            log("FAIL: subprocess exited after first result — pattern 2 NOT viable")
            return 1

        # Frame 2 — without closing stdin
        log("=== writing frame 2 (without closing stdin) ===")
        frame2 = {"type":"user","message":{"role":"user","content":"What was the secret word? Reply with only the word in caps."}}
        try:
            proc.stdin.write(json.dumps(frame2) + "\n")
            proc.stdin.flush()
        except BrokenPipeError:
            log("FAIL: broken pipe on second write — subprocess closed stdin")
            return 1

        log("reading until second result")
        r2 = read_until_result(proc)
        if r2 is None:
            log("FAIL: no second result")
            return 1

        result2_text = (r2.get("result") or "")
        if "PURPLE" in result2_text.upper():
            log("PASS: second response references PURPLE from frame 1 context")
        else:
            log(f"FAIL: second response did not include PURPLE: {result2_text!r}")
            return 1

        # Frame 3 — verify it really keeps going
        log("=== writing frame 3 ===")
        frame3 = {"type":"user","message":{"role":"user","content":"Reverse that word."}}
        try:
            proc.stdin.write(json.dumps(frame3) + "\n")
            proc.stdin.flush()
        except BrokenPipeError:
            log("FAIL: broken pipe on third write")
            return 1
        r3 = read_until_result(proc)
        if r3 is None:
            log("FAIL: no third result")
            return 1
        result3_text = (r3.get("result") or "")
        if "ELPRUP" in result3_text.upper():
            log("PASS: third response includes reversed word — context fully maintained")
        else:
            log(f"PARTIAL: third response: {result3_text!r}")

        # Clean shutdown
        log("=== closing stdin to trigger clean exit ===")
        proc.stdin.close()
        try:
            rc = proc.wait(timeout=15)
            log(f"subprocess exited cleanly with rc={rc}")
        except subprocess.TimeoutExpired:
            log("FAIL: subprocess did not exit after stdin close, killing")
            proc.kill()
            return 1

        log("=== ALL CHECKS PASSED — pattern 2 is viable ===")
        return 0
    finally:
        if proc.poll() is None:
            proc.kill()


if __name__ == "__main__":
    sys.exit(main())
