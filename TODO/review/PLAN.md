## Shore V2 — Consolidated Change Plan

**Date:** 2026-04-16
**Synthesizes:** `REVIEW.md` (critical review), `STEELMAN.md` (adversarial re-verification + new findings), `GOALS_AUDIT.md` (alignment with stated goals).
**Purpose:** One prioritized, de-duplicated list of proposed changes. Severities reflect STEELMAN's revisions where they differ from REVIEW.

---

### 0. Disposition of disputed findings

| REVIEW finding | Status | Reason |
|---|---|---|
| 2.1 Ephemeral tempdir drop (`shore-mcp/src/server.rs:39`) | **REJECTED — do not change** | `drop(resolved)` runs *after* `service.waiting().await` returns. Control flow was misread. Acting on the REVIEW recommendation would introduce a regression. |
| 3.2 OpenAI/ZAI duplication | **Severity HIGH → MEDIUM** | Documented trade-off in `DECISIONS.md`. Revisit when a 3rd OpenAI-compat provider lands. |
| 3.3 `autonomy/manager.rs` size | **Severity HIGH → MEDIUM** | Clean public facade; extract state layer first for testability, not full split. |
| 2.4 `AudioPlayer::finish()` | **Scope narrowed to TUI** | `shore-cli` already calls `wait_until_done()`. Bug is TUI-only. |

---

### 1. MUST FIX — confirmed bugs, cheap

1. **Discovery fallback string mismatch** — `shore-client/src/discovery.rs:179`. Change `"instances registry is empty"` → `"instances file is empty"`. Prefer replacing string-matching with a typed `ClientError::Discovery` variant so the compiler catches future drift. *(REVIEW 2.2)*
2. **Gemini non-streaming tool_use IDs not unique** — `shore-llm-client/src/providers/gemini.rs:590`. Use `format!("gemini_call_{i}")` to match the streaming path. *(REVIEW 2.3)*
3. **Image upload filename not sanitized** — `shore-daemon/src/handler/images.rs`. Reject `/`, `\`, `..`, and non-UTF-8 at the wire boundary before `PathBuf::join`. Latent path-traversal write primitive. *(STEELMAN N4)*
4. **`autonomy_state.json` written non-atomically** — `shore-daemon/src/autonomy/manager.rs` (`save_state`). Switch `std::fs::write` → existing `atomic_write` (write-tmp-then-rename). *(STEELMAN N11)*
5. **JSONL corruption silently swallowed** — `shore-daemon/src/engine/messages.rs` loader. Emit `tracing::error!` with line number on `serde_json::from_str` failure; recover deterministically. *(STEELMAN N5)*
6. **Dead `$?` after `|| true`** — `scripts/live-tests/live-test.sh:213-214`. Trivial; fix exit-code-OR-grep check. *(REVIEW 2.5)*

---

### 2. SHOULD FIX — design issues, higher effort

7. **Release engine lock before inline compaction** — `shore-daemon/src/handler/task.rs:366-414`. Move the LLM-bound compaction run outside the `engine_arc.lock().await` region; re-acquire only for the mutation phase. Establishes "don't hold locks across awaits" as an invariant. *(REVIEW 3.1)*
8. **Interiority tick dropped-future semantics** — `shore-daemon/src/autonomy/manager.rs:843-871`. When `tokio::time::timeout` cancels a tick mid-tool-execution, side effects + state writes race. Enforce timeout inside the LLM call but not across tool dispatch; checkpoint tool executions to a WAL before running them. *(STEELMAN N2)*
9. **Non-atomic autonomy side-effect + state-write pair** — same file, `save_state`. Order `save_state()` before side-effect emission for idempotent actions; use a WAL for non-idempotent ones. *(STEELMAN N3)*
10. **SSE chunk buffering in all streaming providers** — `shore-llm-client/src/providers/{openai,zai,gemini,anthropic}.rs`. Current parsers assume one `data:` event = one complete JSON object. Upstream proxies may fragment large tool_use payloads; combined with silent-drop (REVIEW 4.2) this is a truncation bug. Buffer until terminator; do not per-line parse. Also add `tracing::warn!` on drop. *(STEELMAN N8 + REVIEW 4.2)*
11. **TCP connect timeout in `shore-client`** — `shore-client/src/connection.rs:38`. `TcpStream::connect` has no timeout; blocks 30-120s on dead hosts. Critical for autospawn paths. *(REVIEW 4.1)*
12. **Config validation gaps** — `shore-config`. (a) `daemon.addr` must parse as `SocketAddr`; (b) `CompactionConfig` cross-field invariants (`min_turns <= max_turns`, `keep_recent_turns < max_turns`); (c) `search_depth` must be `"basic"` or `"advanced"`; (d) validate `defaults.compaction` / `defaults.interiority` model refs at load time. *(REVIEW 4.3)*

---

### 3. WORTH DOING — medium impact

13. **`AudioPlayer::finish()` in TUI** — `shore-tui/src/app.rs:567`. Either rename the method and call `wait_until_done()` off-thread from the TUI, or make `finish()` drain (but not on the UI thread). *(REVIEW 2.4, narrowed)*
14. **`memory_shell_sessions` take/put pattern** — `shore-daemon/src/handler/command_dispatch.rs:96-103,170-177`. Convert to `&mut` borrow through `CommandContext` so a future early return can't lose active sessions. *(REVIEW 4.8)*
15. **MCP gate refusals use wrong error code** — `shore-mcp/src/handler.rs:67`. Policy refusal returned as `ErrorData::internal_error`. Use an implementation-defined code or `InvalidParams (-32602)`. *(REVIEW 4.11)*
16. **`conn_manager` zombie reconnect loop** — `shore-client/src/conn_manager.rs:94,117,136-138`. Replace `let _ = event_tx.send(...)` with error checks; trigger graceful shutdown when receiver is dropped. *(REVIEW 4.12 + STEELMAN N7)*
17. **Ledger pricing cache TTL + single-fetch reuse** — `shore-ledger/src/pricing.rs:166-234`. Add ~24h TTL with background refresh; cache the full catalog once per fetch rather than re-downloading per uncached model. *(REVIEW 4.6 + STEELMAN N10)*
18. **Replace `std::env::set_var` in async multi-threaded contexts** — `shore-test-harness/src/config.rs:102`, `shore-mcp/src/profile.rs:105-107`. Move to config-level API key injection; `set_var` is `unsafe` in Rust 2024. *(REVIEW 4.4)*
19. **Synchronous file I/O in async handlers** — `shore-daemon/src/handler/images.rs:48,117,195-196` and `engine/messages.rs:209-223`. Wrap in `spawn_blocking`; `atomic_write` on the hot path compounds with the engine-lock issue (#7). *(REVIEW 4.5)*
20. **TOCTOU in `ensure_state_with_config`** — `shore-daemon/src/autonomy/manager.rs:339`. Use `entry()` API. Zero extra cost. *(REVIEW 4.7)*
21. **Add `thinking` block support to test harness mock** — `shore-test-harness/src/mock_llm.rs`. `AnthropicStreamBuilder` needs thinking; cache tests can't reproduce extended-thinking paths without it. *(REVIEW 4.9)*
22. **Remove user-specific `.env` / `$HOME/Desktop` paths from committed scripts** — `scripts/live-tests/autonomy-test.sh:62-67`, `scripts/cache-tests/22-compaction.sh:78`, `scripts/cache-tests/keepalive-24h.sh:22`. *(REVIEW 4.10)*

---

### 4. STRUCTURAL / ARCHITECTURAL

23. **Break up `shore-daemon` (~35K LOC, 7× the 2–5K crate budget)** — biggest drift from the project's own stated architecture.
    - First cut: extract autonomy state-persistence layer out of `autonomy/manager.rs` (2,841 LOC) into `state.rs`. Lowest coupling, highest testability win. *(GOALS_AUDIT + STEELMAN 3.3)*
    - Second: extract `interiority_executor.rs` and `tick.rs` once state-persistence is isolated.
    - Treat the 2–5K/500 LOC targets as enforceable budgets, not aspirations.
24. **Extract OpenAI-compatible helpers** (deferred) — only when a 3rd OpenAI-compat provider lands. Extract SSE parsing + message translation (excluding reasoning) into `stream_helpers.rs`; keep provider-specific wrappers thin. *(REVIEW 3.2, downgraded)*
25. **Add mock-HTTP integration tests for at least one LLM provider** — per the revised testing policy (Rule 4: recorded fixtures over hand-written stand-ins), record a real Haiku response once and replay. *(REVIEW §7)*
26. **Retry replayability contract** — `shore-llm-client/src/retry.rs`. Change the retry layer's signature from `Request` to `Fn() -> Request` so a non-replayable body becomes a compile error. *(STEELMAN N9)*
27. **Tick shutdown via `JoinSet` with bounded timeout** — `shore-daemon/src/autonomy/manager.rs:327-437`. Spawned tick tasks need awaited shutdown so the process doesn't exit with an open HTTP connection mid-LLM-call. *(STEELMAN N12)*

---

### 5. LOW PRIORITY — stringly-typed protocol, tidy-ups

- Enums/newtypes for `Phase.phase`, `StreamChunk.content_type`, `search_depth`; newtype for `Message.timestamp`. *(REVIEW 5.2)*
- Missing DB index on `ledger.calls.call_type`. *(REVIEW §6)*
- Consolidate error types in generation pipeline (`Box<dyn Error>` vs `(ErrorCode, String)` tuples). *(REVIEW 5.2)*
- Configurable capacity for `shore-diagnostics::Diagnostics`. *(REVIEW §6)*
- Generation task abort not awaited — `shore-daemon/src/handler/mod.rs:401-403`. Low but real concurrent-write risk. *(STEELMAN N6)*
- Misc: `unwrap()` → `expect()` in autonomy; `eprintln!` → `tracing::warn!` in `shore-client/src/client_config.rs`; `serde_json` duplicated in deps/dev-deps in `shore-protocol`; stale duration comment; DIACRITICS table + word-wrap duplication in `shore-tui`; TUI image cols not clamped to 255; API key in Gemini URL appears in logs. *(REVIEW §6)*

---

### 6. Sequencing recommendation

- **Batch A (one PR each, low-risk wins):** items 1, 2, 3, 4, 5, 6. All are < 50 LOC fixes with clear scope.
- **Batch B (design invariants):** item 7 first (unblocks #19), then 10, then 11, 12.
- **Batch C (autonomy correctness):** items 8, 9 together — they share the WAL primitive. Land after #4 so atomic writes already exist.
- **Batch D (structural):** item 23 staged — extract state-persistence, verify via live MCP (per `CLAUDE.md` MCP verification policy), then extract executor/tick.
- **Verification gate for each batch:** `cargo check --workspace`, `cargo test --workspace`, and a live MCP drive-through (`cargo mcp-itest` or manual `shore-mcp` session with Haiku) per the project's "verify with real binaries" priority.

---

### 7. Explicit non-goals

- Do **not** "fix" `shore-mcp/src/server.rs:39` — that would reintroduce a regression (see §0).
- Do **not** unify OpenAI + ZAI providers yet — defer per DECISIONS.md trade-off.
- Do **not** attempt the full autonomy module split in one PR — start with state persistence only.
