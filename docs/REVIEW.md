# Shore V2 — Critical Code Review

**Date:** 2026-04-16
**Scope:** Full workspace — 12 active crates, 84,752 lines of Rust
**Reviewer:** Automated critical review

---

## Executive Summary

Shore V2 is a well-architected modular AI character engine in Rust. The workspace is cleanly separated into 12 crates with hard boundaries, a formalized wire protocol (SWP), and strong documentation culture (ARCHITECTURE.md, DECISIONS.md, QUIRKS.md). The overall code quality is high — consistent use of `thiserror`, proper serde patterns, good test coverage on core paths, and a clear state-ownership model.

This review identifies **5 confirmed bugs**, **3 high-severity design issues**, **12 medium-severity concerns**, and a set of lower-priority items. The most critical findings are: an ephemeral-profile tempdir lifecycle bug in shore-mcp, a string-mismatch in the client discovery fallback, non-unique tool IDs in the Gemini provider, significant code duplication between the OpenAI and ZAI provider modules, and several files far exceeding the project's own 500-LOC guideline.

---

## 1. Codebase Overview

| Crate | LOC | Role |
|-------|-----|------|
| shore-daemon | 40,167 | Core daemon (engine, memory, autonomy, tools, commands) |
| shore-llm-client | 9,446 | LLM provider integrations (Anthropic, OpenAI, Gemini, ZAI) |
| shore-tui | 6,370 | Terminal UI (ratatui) |
| shore-cli | 6,204 | CLI client |
| shore-config | 3,950 | Configuration loading, validation, model catalog |
| shore-protocol | 3,335 | SWP message types and wire protocol |
| shore-ledger | 3,129 | Token usage ledger (SQLite) |
| shore-client | 2,356 | SWP client library, connection management |
| shore-mcp | 2,196 | Debug-only MCP server |
| shore-daemon-server | 1,992 | SWP server and instance registry |
| shore-test-harness | 1,387 | In-process daemon test harness (wiremock) |
| shore-diagnostics | 268 | Ring buffer diagnostics |

**shore-matrix** is disabled in the workspace (`matrix-sdk 0.16.0` hits `recursion_limit` on rustc 1.94+).

### File Size Distribution (files >1000 LOC)

The architecture guideline is ~500 LOC per module. Seven daemon files and several others significantly exceed this:

| File | LOC | Guideline Ratio |
|------|-----|----------------|
| `shore-daemon/src/autonomy/manager.rs` | 2,841 | 5.7x |
| `shore-tui/src/ui.rs` | 2,340 | 4.7x |
| `shore-llm-client/src/providers/anthropic.rs` | 2,190 | 4.4x |
| `shore-daemon/src/memory/collation/mod.rs` | 1,994 | 4.0x |
| `shore-daemon/src/engine/prompt.rs` | 1,818 | 3.6x |
| `shore-daemon-server/src/lib.rs` | 1,663 | 3.3x |
| `shore-daemon/src/memory/db.rs` | 1,457 | 2.9x |
| `shore-daemon/src/memory/compaction/mod.rs` | 1,451 | 2.9x |
| `shore-daemon/src/compat.rs` | 1,257 | 2.5x |
| `shore-llm-client/src/providers/openai.rs` | 1,224 | 2.4x |

---

## 2. Confirmed Bugs

### 2.1 Ephemeral tempdir dropped while MCP service is running (shore-mcp)

**File:** `shore-mcp/src/server.rs:39`

```rust
// Keep the ephemeral tempdir alive for the lifetime of the server.
drop(resolved);
```

The comment says "keep alive" but `drop(resolved)` destroys the `ResolvedProfile` and its `TempDir` immediately. For `--ephemeral` mode, this deletes the daemon's data/runtime directories (config, memory DB, instances.json) while the daemon is still running. Subsequent file operations by the daemon will fail silently or error.

**Fix:** Hold `resolved.tempdir` alive for the duration of `service.waiting().await`. Remove the early `drop()`.

**Severity:** HIGH

### 2.2 Wrong string in discovery fallback check (shore-client)

**File:** `shore-client/src/discovery.rs:179`

```rust
|| message == "instances registry is empty"  // WRONG
```

The actual error string produced by `discover_from_path` is `"instances file is empty"` (line 122), not `"instances registry is empty"`. This means an empty instances file will NOT fall back to the default address and will instead propagate as an error to the caller. shore-mcp has the correct string in its own copy (`"instances file is empty"` at `profile.rs:151`).

**Fix:** Change to `message == "instances file is empty"`.

**Severity:** HIGH

### 2.3 Gemini non-streaming tool_use IDs are not unique (shore-llm-client)

**File:** `shore-llm-client/src/providers/gemini.rs:590`

```rust
id: format!("gemini_{name}")
```

Tool IDs use the tool **name** as the identifier. If a response contains two calls to the same tool (e.g., two `search` calls), both get `id: "gemini_search"`, violating the uniqueness constraint. This causes incorrect tool_result matching in subsequent turns. The streaming path correctly uses `format!("gemini_call_{i}")`.

**Fix:** Use `format!("gemini_call_{i}")` like the streaming path.

**Severity:** HIGH

### 2.4 AudioPlayer::finish() claims to drain but is a no-op (shore-client)

**File:** `shore-client/src/audio.rs:87-89`

```rust
pub fn finish(&self) {
    debug!("Audio stream finished, draining buffer");
}
```

The log message says "draining buffer" but the function does nothing. Both shore-tui and shore-cli call `player.finish()` expecting it to wait for playback to complete. A separate `wait_until_done()` method exists but is never called by consumers.

**Fix:** Either call `sink.sleep_until_end()` inside `finish()` when a sink exists, or rename to `mark_finished()` and document that it does not wait.

**Severity:** MEDIUM

### 2.5 `$?` capture after `|| true` is dead code (scripts)

**File:** `scripts/live-tests/live-test.sh:214`

```bash
output=$(timeout 30 $CLI regen 2>&1) || true
if [[ $? -eq 0 ]] || echo "$output" | grep -qiF "PONG\|test\|response"; then
```

After `|| true`, `$?` is always 0, making the `[[ $? -eq 0 ]]` check dead code. The test always falls through to the `grep` branch. The intent (check exit code OR check output) is not implemented correctly.

**Severity:** LOW

---

## 3. High-Severity Design Issues

### 3.1 Engine lock held during inline compaction

**File:** `shore-daemon/src/handler/task.rs:366-414`

After persisting a response, compaction runs inline while holding `engine_arc.lock().await`. Compaction includes LLM calls (seconds of latency). During this window, any other session trying to send a message to the same character will block. This is partially mitigated by per-session generation serialization, but it's a significant latency spike.

**Recommendation:** Release the engine lock before compaction, or move compaction to a spawned task that re-acquires the lock only for the mutation phase.

### 3.2 Significant code duplication between OpenAI and ZAI providers

**Files:** `shore-llm-client/src/providers/openai.rs` and `zai.rs`

| Function | OpenAI LOC | ZAI LOC | Similarity |
|----------|-----------|---------|------------|
| `translate_messages()` | 170 lines | 177 lines | ~95% |
| Streaming SSE callback | 170 lines | 168 lines | ~95% |
| `generate()` | 112 lines | 109 lines | ~95% |
| `build_headers()` | 30 lines | 19 lines | ~90% |

The only meaningful difference is ZAI's `reasoning_content` handling and `clear_thinking` flag. ~450 lines are essentially copy-pasted. This will inevitably diverge over time.

**Recommendation:** Extract shared OpenAI-compatible logic into `stream_helpers.rs` with a parameter struct for ZAI-specific behavior.

### 3.3 `autonomy/manager.rs` at 2,841 LOC — god module

The file is 5.7x the 500-LOC guideline and handles: state persistence, tick scheduling, compaction triggering, interiority execution, cache keepalive coordination, debug commands, background task management, and daemon lifecycle hooks. These are 5-6 distinct concerns that should be separate modules.

**Recommendation:** Extract into `state.rs`, `tick.rs`, `compaction_trigger.rs`, `interiority_executor.rs`, and `lifecycle.rs`.

---

## 4. Medium-Severity Concerns

### 4.1 No TCP connect timeout (shore-client)

**File:** `shore-client/src/connection.rs:38`

`TcpStream::connect(&addr.0)` has no timeout. Connecting to a non-routable IP or silently-dropping host blocks for the OS TCP timeout (30-120 seconds).

### 4.2 Silent SSE chunk dropping in streaming providers

**Files:** `openai.rs:347-349`, `zai.rs:342-344`, `gemini.rs:409`, `anthropic.rs:782`

Malformed JSON in SSE data chunks is silently dropped with no logging. A corrupted response chunk mid-stream disappears with no error trace, making debugging very difficult.

**Recommendation:** Add `tracing::warn!` for each dropped chunk before returning `None`.

### 4.3 Config validation gaps (shore-config)

| Gap | File | Detail |
|-----|------|--------|
| `daemon.addr` not validated as parseable | `app.rs:59` | Typo like `"127.0.0.1"` (no port) fails only at daemon bind |
| `CompactionConfig` cross-field invariants | `app.rs:222-255` | No check that `min_turns <= max_turns` or `keep_recent_turns < max_turns` |
| `search_depth` accepts any string | `app.rs:401` | Only "basic" and "advanced" are valid for Tavily |
| `defaults.compaction` / `defaults.interiority` model refs not validated | `lib.rs:445-455` | Typos surface only at runtime |

### 4.4 `std::env::set_var` in multi-threaded context

**Files:** `shore-test-harness/src/config.rs:102`, `shore-mcp/src/profile.rs:105-107`

`set_var` is not thread-safe and is marked unsafe in Rust 2024. Called inside async functions on a multi-threaded Tokio runtime. In practice the timing is safe (runs before concurrent access), but it's architecturally unsound.

### 4.5 Synchronous file I/O in async context (shore-daemon)

**Files:** `handler/images.rs:48,117,195-196`, `engine/messages.rs:209-223`

- `std::fs::read`/`write`/`copy` called directly in async handlers for image uploads
- `atomic_write` in `persist()` rewrites entire JSONL conversation file synchronously while holding the engine lock
- SQLite operations via `MemoryDB` called synchronously from async tool handlers

### 4.6 `fetch_and_cache_catalog` downloads full catalog per uncached model (shore-ledger)

**File:** `shore-ledger/src/pricing.rs:166-234`

Every pricing fetch downloads the full OpenRouter model catalog (~1000+ models, ~1MB JSON). If two models are uncached in sequence, the catalog is downloaded twice. No staleness check — cached pricing is used forever regardless of age.

### 4.7 TOCTOU in autonomy state creation

**File:** `shore-daemon/src/autonomy/manager.rs:339`

`ensure_state_with_config` uses `contains_key` then `insert` (not `entry` API), which has a TOCTOU race if called concurrently for the same character. Currently safe because all calls serialize through the handler loop, but fragile for future changes.

### 4.8 `memory_shell_sessions` take-and-put pattern

**File:** `shore-daemon/src/handler/command_dispatch.rs:96-103,170-177`

The session's `memory_shell_sessions` HashMap is taken out of `SessionState`, placed in `CommandContext`, then put back after the command. Any future early return added between the take and put-back will silently lose all active memory shell sessions.

### 4.9 No thinking block support in test harness mock

**File:** `shore-test-harness/src/mock_llm.rs`

`AnthropicStreamBuilder` supports `Text` and `ToolUse` but not `thinking` blocks. Several cache tests use thinking-enabled configurations, but the in-Rust mock cannot reproduce this. Tests involving extended thinking are unreachable via the harness.

### 4.10 User-specific `.env` path committed to repository

**File:** `scripts/live-tests/autonomy-test.sh:62-67`, `scripts/cache-tests/22-compaction.sh:78`

```bash
ENV_FILE="${SHORE_ENV_FILE:-$HOME/Documents/qifei/config/.env}"
```

Hardcoded user-specific path in a shared repository. The `SHORE_ENV_FILE` override exists, but the default references a private directory.

### 4.11 Gate refusals return `internal_error` instead of client-appropriate code (shore-mcp)

**File:** `shore-mcp/src/handler.rs:67`

When mutation tools refuse to execute on the main profile (no `--allow-main-writes`), the error is returned as `ErrorData::internal_error`. This is misleading — it's not an internal error, it's a policy refusal. MCP clients may display "Internal Server Error" to the user.

### 4.12 `conn_manager` ignores send failures, risking zombie reconnect loops (shore-client)

**File:** `shore-client/src/conn_manager.rs:94,117,136-138`

All `event_tx.send()` calls use `let _ =`, silently discarding send errors. If the event receiver is dropped but the command sender is not, the connection loop will reconnect indefinitely, sending events into the void.

---

## 5. Architectural Observations

### 5.1 Strengths

- **Formalized wire protocol.** SWP is well-documented with golden-file tests, version negotiation, and clear state-ownership rules. Adding new clients or bridges requires zero daemon changes.
- **Protocol crate separation.** `shore-protocol` is lean (~330 LOC production code) with excellent serde hygiene: tagged enums, proper defaults, forward-compatibility tested, `skip_serializing_if` throughout.
- **Documentation culture.** ARCHITECTURE.md, DECISIONS.md, and QUIRKS.md are comprehensive and kept current. Every major design decision is recorded with rationale and trade-offs.
- **Error handling.** Consistent use of `thiserror` for typed errors. The `lock_or_recover` pattern for poisoned mutexes is correct and well-tested.
- **Test infrastructure.** The wiremock-based `TestHarness` boots a real daemon in-process with mock HTTP. This is a strong testing strategy that exercises all daemon plumbing.
- **Compiler-enforced ledger recording.** `LedgerClient` consumes `LlmClient`, making it structurally impossible to make an unlogged API call. Excellent type-system leverage.
- **State ownership model.** The four-tier state hierarchy (daemon-global, session, character, request-local) is documented and generally followed.

### 5.2 Weaknesses

- **Daemon module sizes.** 7 files exceed 1000 LOC. The 500-LOC guideline exists but is not enforced. `autonomy/manager.rs` at 2,841 LOC is the worst offender.
- **God-object `CharacterRegistry`.** A single `Arc<tokio::sync::Mutex<CharacterRegistry>>` guards engines, memory DBs, vector stores, and character configs. Fine-grained locking would reduce contention.
- **Provider code duplication.** OpenAI and ZAI are ~450 lines of copy-paste. The DECISIONS.md entry for the SDK/provider split explicitly acknowledges this trade-off ("accepted because a shared abstraction would need to accommodate three different thinking parameter formats"). The trade-off is reasonable now but will become painful as providers add features.
- **Stringly-typed protocol fields.** `Phase.phase`, `StreamChunk.content_type`, `Message.timestamp`, and `search_depth` are all `String` where enums or newtypes would provide compile-time safety. The project's own directive says "prefer compiler-enforced correctness over runtime checks."
- **`Box<dyn Error>` in generation pipeline.** The daemon's generation/persistence errors use boxed trait objects rather than typed enums. Commands use `(ErrorCode, String)` tuples. Two different error conventions coexist.

---

## 6. Lower-Priority Items

### Code Quality

| Item | File | Detail |
|------|------|--------|
| `unwrap()` on chrono duration conversion | `autonomy/manager.rs:122,124` | Should use `expect()` with a message |
| `unwrap()` on `next_wake_at` | `autonomy/interiority.rs:188,274` | Safe by construction but should use `expect()` |
| `eprintln!` instead of `tracing::warn!` | `shore-client/src/client_config.rs:32,39` | Bypasses tracing subscriber |
| `as_ref().unwrap()` after `Some()` assignment | `autonomy/activity.rs:165,176` | Safe but verbose; use `insert()` returning `&T` |
| Stale comment about fractional durations | `shore-config/src/duration.rs:258-260` | Claims they fail; they actually work |
| Silent failure on image file read | `shore-client/src/connection.rs:204-215` | No warning for nonexistent image paths |
| API key in Gemini URL | `shore-llm-client/src/providers/gemini.rs:372-377` | Standard Gemini auth, but appears in logs |
| `serde_json` duplicated in deps + dev-deps | `shore-protocol/Cargo.toml:8,11` | Redundant |
| `derive_content_from_blocks_with` is `pub` but never used externally | `shore-protocol/src/types.rs:156` | Should be `pub(crate)` |
| DIACRITICS table duplicated | `shore-tui/src/images.rs:94` + `kitty_diag.rs:510` | Should share the table |
| Two word-wrap implementations | `shore-tui/src/app.rs:284` + `ui.rs:177` | Different algorithms must stay in sync |
| TUI image cols not clamped to 255 | `shore-tui/src/images.rs:276` | Rows clamped but not cols; diacritics wrap for very wide images |
| Wire inconsistency: `delete` refs type | `shore-cli/src/cli.rs:170` | CLI sends string; TUI sends string or array |
| `ImageRef::PartialEq` ignores `data` field | `shore-protocol/src/types.rs:23-27` | Undocumented rationale |
| `NewMessage` uses `#[serde(flatten)]` | `shore-protocol/src/server_msg.rs:100` | Fragile if `Message` gains a `revision` field |
| Missing index on `call_type` column | `shore-ledger/src/ledger.rs:46-49` | Usage queries GROUP BY call_type |
| `cache_ttl` NULL vs `'1h'` inconsistency | `shore-ledger/src/ledger.rs:23` | Schema default never used; insert always provides value |
| `normalize_anthropic_model` fragile | `shore-ledger/src/pricing.rs:335-348` | Only replaces last digit-hyphen-digit |
| `Diagnostics` capacity hardcoded | `shore-diagnostics/src/lib.rs` | No constructor to configure ring buffer size |

### Scripts

| Item | File | Detail |
|------|------|--------|
| User-specific `$HOME/Desktop` path | `scripts/cache-tests/keepalive-24h.sh:22` | Won't exist on headless CI |
| Hardcoded `/tmp` paths | `scripts/test-daemon.sh:13-14` | Collides if multiple users run it |
| `sed -i` not portable to macOS | `scripts/cache-tests/harness.sh:272` | GNU sed vs BSD sed difference |
| `python3` availability not checked | `scripts/cache-tests/harness.sh:138` | Silent empty-string output on failure |
| `_write_config` duplicated across 4 cache scripts | `scripts/cache-tests/20-23*.sh` | Should parameterize harness.sh's version |
| `run_test_contains` truncates to 5 lines | `scripts/live-tests/live-test.sh:54` | Loses diagnostic context on failure |
| `test-daemon.sh` zombie process handling | `scripts/test-daemon.sh:43-58` | `kill -0` succeeds on zombies |
| `experiment.py` extremely long single lines | `scripts/cache-tests/experiment.py:94-109` | ~200 char lines, unreadable |

---

## 7. Test Coverage Assessment

| Area | Coverage | Notes |
|------|----------|-------|
| shore-protocol | Excellent | 1034-line golden test file, serde roundtrips, forward-compat |
| shore-llm-client (unit) | Good | 100+ unit tests across providers, SSE parser, stream consumer, retry |
| shore-llm-client (integration) | Weak | No mock-HTTP end-to-end tests for any provider |
| shore-daemon (unit) | Moderate | Tests per module, but many paths untested outside e2e |
| shore-daemon (e2e) | Good | 1023-line e2e test file + live tests with real APIs |
| shore-config | Good | Duration parsing, config validation, model resolution |
| shore-client | Moderate | Handshake, stream, discovery tests in lib.rs |
| shore-tui | Good | 80 tests including scenario-based rendering tests |
| shore-cli | Moderate | CLI parsing tested; output formatting less so |
| shore-ledger | Moderate | DB operations, pricing, stream; no edge-case tests |
| shore-mcp | Moderate | Integration tests exist; autospawn detach tested |
| Missing | — | `embed()`, `image_generate()`, `cache_forensics` untested |
| Missing | — | No test for partial/incremental tool call JSON across SSE chunks |
| Missing | — | No test for concurrent state mutations on same character |

---

## 8. Recommendations (Prioritized)

### Must Fix (Bugs)
1. Fix ephemeral tempdir drop in `shore-mcp/src/server.rs:39`
2. Fix discovery fallback string in `shore-client/src/discovery.rs:179`
3. Fix Gemini non-streaming tool_use IDs in `shore-llm-client/src/providers/gemini.rs:590`

### Should Fix (High Impact)
4. Extract OpenAI/ZAI shared logic (~450 lines of duplication)
5. Release engine lock before inline compaction
6. Split `autonomy/manager.rs` (2,841 LOC) into focused modules
7. Add SSE chunk drop logging in all provider streaming callbacks
8. Fix `AudioPlayer::finish()` to actually drain or document no-wait behavior

### Worth Doing (Medium Impact)
9. Add TCP connect timeout to `shore-client`
10. Add missing config validations (address format, compaction invariants, model refs)
11. Add `thinking` block support to test harness mock
12. Replace `std::env::set_var` with config-level API key injection
13. Add staleness check to ledger pricing cache
14. Remove user-specific paths from committed scripts
15. Add mock-HTTP integration tests for at least one LLM provider
16. Address `conn_manager` zombie loop on dropped receiver

### Nice to Have (Low Impact)
17. Use enums for `Phase.phase`, `StreamChunk.content_type`, `search_depth`
18. Use newtype for `Message.timestamp`
19. Add missing DB index on `ledger.calls.call_type`
20. Consolidate error types in generation pipeline
21. Move sync file I/O to `spawn_blocking` in image handler
22. Add configurable capacity to `shore-diagnostics::Diagnostics`
