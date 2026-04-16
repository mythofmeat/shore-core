# Shore V2 — Adversarial Steelman of REVIEW.md

**Date:** 2026-04-16
**Input:** `TODO/review/REVIEW.md`
**Method:** Each finding re-verified against current code. Adversarial counter-arguments considered. New findings added from independent pass.

---

## Executive Summary

REVIEW.md is high-quality overall but contains **one hard false positive (2.1)** and **several imprecisions in severity or framing**. Of the 5 "confirmed bugs," 3 are real, 1 is partial, and 1 is refuted. The design-issue section is largely correct but overstates severity in one place (3.2 is a documented, accepted trade-off).

A separate adversarial pass surfaced **12 new findings** the review missed, clustered around dropped-future semantics in the autonomy tick, non-atomic state persistence, and path-traversal hygiene in image handling.

---

## 1. Re-verification of "Confirmed Bugs" (Section 2)

### 2.1 Ephemeral tempdir drop — **REFUTED**

The review claims `drop(resolved)` destroys the TempDir **while the daemon is running**. This misreads the control flow.

`shore-mcp/src/server.rs`:

```rust
33    service
34        .waiting()
35        .await
36        .map_err(...)?;
37
38    // Keep the ephemeral tempdir alive for the lifetime of the server.
39    drop(resolved);
```

`service.waiting().await` blocks until the MCP client closes the connection. `drop(resolved)` runs **after** that — i.e., after the server has stopped serving. The explicit `drop()` is a defensive hold (preventing the compiler from dropping `resolved` earlier via NLL). The comment is accurate; the code is correct.

**Disposition:** Reject this finding. Do not change the code.

**Minor nit still applicable:** The explicit `drop()` is redundant — `resolved` is a local that naturally lives to function end. Removing it or replacing with `let _resolved = resolved;` would be equally correct. Not a bug.

### 2.2 Discovery fallback string mismatch — **CONFIRMED**

Verified:
- `shore-client/src/discovery.rs:122` produces `"instances file is empty"`
- `shore-client/src/discovery.rs:179` matches `"instances registry is empty"`

These strings do not match. An empty instances file propagates as a discovery error instead of falling back to `DEFAULT_ADDR`. Severity **HIGH** is appropriate for UX impact on fresh installs.

**Steelman counter considered:** Could the "empty" case be intentionally non-fallback (e.g., to distinguish from "no file at all")? No — the adjacent `starts_with("instances registry not found at ")` branch already handles the missing-file case and falls back. The empty-file case is logically parallel. This is a typo, not a design choice.

**Fix:** Change line 179 to `"instances file is empty"`, or (better) replace the string-matching with a typed variant of `ClientError::Discovery` so the compiler catches future drift.

### 2.3 Gemini non-streaming tool IDs — **CONFIRMED**

Verified:
- Non-streaming (`gemini.rs:590`): `id: format!("gemini_{name}")`
- Streaming (`gemini.rs:495`): `format!("gemini_call_{i}")`

Two calls to the same tool in one response produce identical IDs non-streaming; streaming uses index-based IDs.

**Steelman counter considered:** Does Gemini actually emit duplicate tool calls in a single turn? The SDK contracts don't forbid it, and prompt-chained patterns (e.g., parallel search) make it plausible. Anthropic and OpenAI both assume IDs are unique; code downstream of the provider almost certainly relies on this.

**Severity HIGH** is appropriate.

**Fix:** Mirror the streaming path's `format!("gemini_call_{i}")`.

### 2.4 AudioPlayer::finish() — **PARTIAL**

The review is correct that `finish()` logs "draining buffer" but does nothing. Callsite analysis:

- `shore-cli/src/run.rs:845` — calls `finish()` **and then** `wait_until_done()` (correct behavior)
- `shore-tui/src/app.rs:567` — calls only `finish()` (buffer never actually drained)

So the **bug is localized to the TUI**, not a universal problem. The fix is correct (either make `finish()` drain, or rename — I'd pick rename, because draining on the UI thread is unacceptable anyway; the TUI callsite should be the one calling `wait_until_done()` off-thread).

**Severity:** MEDIUM stays appropriate.

### 2.5 Dead `$?` after `|| true` — **CONFIRMED**

`scripts/live-tests/live-test.sh:213-214`:

```bash
output=$(timeout 30 $CLI regen 2>&1) || true
if [[ $? -eq 0 ]] || echo "$output" | grep -qiF "PONG\|test\|response"; then
```

`$?` is always 0 after `|| true`. The test always falls through to the grep. Trivial but real; severity LOW is appropriate.

---

## 2. Re-verification of "High-Severity Design Issues" (Section 3)

### 3.1 Engine lock held during compaction — **CONFIRMED**

`shore-daemon/src/handler/task.rs:366-414` shows a single `engine_arc.lock().await` block that wraps both the compaction decision and execution — including the LLM call at `run_compaction()` (awaited, not spawned). The lock spans seconds of network latency.

**Steelman counter considered:** Compaction is infrequent (idle or turn-count triggered). Per-session serialization already ensures only one user turn per session at a time. So what's the real impact?

Concurrent sessions targeting the **same character** (multi-client, CLI + TUI simultaneously, or autonomy tick + user message) still contend at the engine lock. This is not a hot path but it's a real tail-latency spike. Severity HIGH is justified on architectural grounds: releasing network-bound locks is a cheap fix and a "don't hold locks across awaits" rule is worth enforcing.

### 3.2 OpenAI/ZAI duplication — **CONFIRMED but downgrade severity**

The ~95% duplication claim is accurate for the core translation / SSE / generate paths. However, `docs/DECISIONS.md` explicitly records this as an accepted trade-off: a shared abstraction would need to accommodate three thinking-parameter formats (Anthropic budget_tokens, OpenAI reasoning_effort, Z.AI thinking object), and the authors concluded the abstraction cost exceeds the duplication cost.

**Steelman for the review:** The trade-off decision was made when only these two providers existed. Adding a third OpenAI-compat provider (xAI, Groq, DeepSeek) triples the duplication. A better fix than full unification is extracting format-agnostic helpers (SSE parsing, message translation excluding reasoning) and keeping provider-specific wrappers thin.

**Suggested severity:** downgrade to MEDIUM; re-evaluate if a third OpenAI-compat provider lands.

### 3.3 autonomy/manager.rs 2841 LOC — **CONFIRMED but downgrade severity**

The 8 concerns listed in the review are all present in the file. However:

- The module has a clean public facade (~24 public methods)
- Internal tick logic is block-structured with visible seams
- The concerns share a common tick schedule and `TickContext`, so extracting them means adding dependency plumbing without architectural gain

**Steelman for the review:** Even within a single use case, 2,841 LOC is beyond what one human can reliably reason about, and the 500 LOC guideline exists for a reason. The strongest argument for splitting is **testability**: extracting `state.rs` and `interiority_executor.rs` would let each be tested in isolation without booting the full tick loop.

**Suggested severity:** MEDIUM. Extract the state-persistence layer first (lowest coupling, highest testability win).

---

## 3. Re-verification of Medium Concerns (Section 4)

All 12 items CONFIRMED. No corrections. Key calibration notes:

- **4.1 (TCP connect timeout):** confirm no timeout wrapper on `TcpStream::connect`. Severity MEDIUM appropriate for UX, arguably HIGH for autospawn paths where a stalled connect blocks daemon startup.
- **4.3 (config validation):** all four gaps real. (a), (c), (d) are one-line fixes; (b) is a real invariant.
- **4.5 (sync I/O in async):** `atomic_write` in `persist()` on the hot path is the worst of these — it blocks the engine lock (see 3.1) on disk I/O in addition to the LLM call.
- **4.7 (TOCTOU):** Currently safe because handler serializes through the tick loop, but the `entry()` API is literally zero extra cost. Fix unconditionally.
- **4.8 (memory_shell_sessions take/put):** genuinely fragile. The steelman fix is to make it `&mut` borrow rather than take/put.
- **4.11 (internal_error for policy refusal):** MCP spec has `-32602 InvalidParams` and implementation-defined codes. Using `internal_error` is contract-wrong, not just UX-ugly.

---

## 4. New Findings (Missed by Review)

### 4.N1 `drop(resolved)` in shore-mcp is the *correct* pattern, not a bug — **META**

The review got this backward (see 2.1 above). If this code is "fixed" per the review's recommendation, it becomes broken. Worth noting as a lesson for automated review tooling: `drop()` placement relative to `.await` is semantically load-bearing.

### 4.N2 Dropped interiority tick loses partial state — **HIGH**

**File:** `shore-daemon/src/autonomy/manager.rs:843-871` (interiority tick with `tokio::time::timeout`)

When the timeout fires, the `execute_unified_tick()` future is dropped mid-execution. If an LLM call returned a tool_use and the handler began executing it but hadn't yet persisted results, those mutations are lost. Next tick has no record that the tool ran — potential double-execution of side-effectful tools (memory writes, sends).

**Mitigation ideas:** Tool executions should be checkpointed to a write-ahead log, or timeout should be enforced inside the LLM call but not across tool dispatch.

### 4.N3 Non-atomic autonomy state + side-effect race — **HIGH**

**File:** `shore-daemon/src/autonomy/manager.rs:144-174` (`save_state`)

Interiority actions (ping emission, compaction flag set) produce side effects before `save_state()` is called. A daemon crash between the side effect and the state write causes either double-fire (action repeated on restart) or missed-completion tracking. No two-phase commit.

**Fix:** Order `save_state()` *before* emitting the side effect for idempotent actions; for non-idempotent actions, use a WAL.

### 4.N4 Image upload path does not sanitize `filename` — **MEDIUM (latent HIGH)**

**File:** `shore-daemon/src/handler/images.rs:114` (approximate)

Client-supplied `upload.filename` is used in path construction. `PathBuf::join` with an absolute path silently replaces the base, and `..` components are not pre-stripped. Current callsite may be safe, but this is one refactor away from a path-traversal write primitive. Validate at the wire boundary: reject `/`, `\`, `..`, and non-UTF-8 bytes.

### 4.N5 Malformed JSONL lines silently skipped in message recovery — **MEDIUM**

**File:** `shore-daemon/src/engine/messages.rs` (JSONL loader)

`serde_json::from_str()` on each line; failed lines are silently dropped. If a corrupted line appears mid-file (power loss, partial write), the user loses conversation turns with no warning. Should emit `tracing::error!` with line number and truncate/recover deterministically.

### 4.N6 Generation task abort does not await completion — **LOW/MEDIUM**

**File:** `shore-daemon/src/handler/mod.rs:401-403` (approximate)

When a new generation supersedes an old one, `prev.abort()` is called but the handle is not `.await`ed. The aborted task may still be mid-flight in the HTTP client. New generation starts immediately. Risk: concurrent writes to the same session state file, or racing ledger entries.

### 4.N7 `conn_manager` channel send failures silently ignored — **MEDIUM** (expands on review 4.12)

The review flagged this but understated a second failure mode: if the receiver is dropped but the command sender is alive, the connection loop burns CPU reconnecting and emitting events into a closed channel indefinitely. Should convert `let _ = event_tx.send()` to check the `Err` and trigger graceful shutdown.

### 4.N8 SSE parser stalls on partial tool_use JSON across chunks — **MEDIUM**

**Files:** all `shore-llm-client/src/providers/*.rs` streaming callbacks

The parsers assume each SSE `data:` chunk is a complete JSON object. If an upstream proxy fragments a large tool_use input across two SSE events (valid per spec), `serde_json::from_str` fails and the chunk is silently dropped (see review 4.2). Combined, this is a silent truncation bug. The fix is chunk buffering until `data:` terminator, not per-line parsing.

### 4.N9 Retry logic assumes request body is replayable — **LOW**

**File:** `shore-llm-client/src/retry.rs`

Current providers construct JSON bodies from owned data, so replay works. But the `retry` layer doesn't *enforce* replayability — a future provider that passes a `reqwest::Body::wrap_stream(...)` will retry with an already-consumed body and fail silently. Add a compile-time check (e.g., `Fn() -> Request` instead of `Request`).

### 4.N10 `pricing.rs` cache persists forever — **LOW** (expands review 4.6)

The review flagged the re-download cost; the additional concern is **staleness**: OpenRouter prices change. Without a TTL, a long-running daemon charges users at prices that may be weeks stale. TTL of ~24h with background refresh is standard.

### 4.N11 `autonomy/manager.rs` writes state via `std::fs::write` (not atomic) — **MEDIUM**

**File:** `shore-daemon/src/autonomy/manager.rs` (`save_state`)

`std::fs::write` truncates then writes. A crash mid-write produces a corrupt/empty `autonomy_state.json`. Elsewhere in the codebase `atomic_write` (write-tmp-then-rename) is used; autonomy state should use the same pattern.

### 4.N12 Spawned tick tasks not tracked for graceful shutdown — **LOW**

**File:** `shore-daemon/src/autonomy/manager.rs:327-437`

Per-character tick tasks are spawned with `tokio::spawn`; handles are tracked but shutdown broadcasts rely on the tasks observing the signal promptly. If a tick is mid-LLM-call, shutdown can race past it, causing the process to exit while the HTTP connection is still open. Should `JoinSet` and await, with a bounded timeout.

---

## 5. Meta-observations on REVIEW.md

**Strengths of the review:**

- Accurate identification of real duplication and real god-modules
- Good coverage of medium-severity items across all crates
- Appropriate use of file:line references
- Useful prioritization table at the end

**Weaknesses:**

- **One false positive (2.1)** presented as HIGH severity — this is the most concerning item because acting on the recommendation would *introduce* a regression. Automated reviewers struggle with control-flow-dependent lifetime reasoning.
- **Severity inflation on 3.2 and 3.3.** Both are documented trade-offs or well-contained modules, not "high-severity." Downgrading these to MEDIUM would better reflect engineering reality.
- **Gaps around dropped-future semantics.** The review doesn't consider what happens when `tokio::time::timeout` cancels an in-flight future — a class of bug that accounts for 3 of the new findings (4.N2, 4.N3, 4.N6).
- **Gaps around atomic persistence.** The review notes sync I/O in async context (4.5) but doesn't ask which writes need atomicity (N11) or two-phase commits (N3).
- **No path-traversal audit** despite image upload being user-input-facing (N4).

---

## 6. Prioritized Action List (Revised)

**MUST FIX (real bugs, cheap):**

1. Discovery fallback string (REVIEW 2.2)
2. Gemini non-streaming tool_use IDs (REVIEW 2.3)
3. Image upload filename sanitization (NEW N4)
4. Atomic write for `autonomy_state.json` (NEW N11)
5. JSONL corruption logging (NEW N5)

**DO NOT FIX (review false positive):**

- REVIEW 2.1 `drop(resolved)` — leave as-is.

**SHOULD FIX (design, higher effort):**

6. Release engine lock before compaction (REVIEW 3.1)
7. Interiority tick checkpointing / WAL for side effects (NEW N2, N3)
8. SSE chunk buffering in provider parsers (NEW N8, REVIEW 4.2)
9. Config validation gaps (REVIEW 4.3)
10. TCP connect timeout in `shore-client` (REVIEW 4.1)

**WORTH DOING:**

11. AudioPlayer::finish() — rename or drain (REVIEW 2.4, TUI-only)
12. `memory_shell_sessions` borrow instead of take/put (REVIEW 4.8)
13. MCP gate refusals: correct error code (REVIEW 4.11)
14. conn_manager send-failure handling (REVIEW 4.12, expanded by N7)
15. Pricing cache TTL (REVIEW 4.6, expanded by N10)

**LOWER PRIORITY:**

- OpenAI/ZAI helper extraction (REVIEW 3.2) — defer until third OpenAI-compat provider
- Autonomy manager split (REVIEW 3.3) — extract state persistence first
- Retry replayability contract (NEW N9)
- Tick shutdown JoinSet (NEW N12)
