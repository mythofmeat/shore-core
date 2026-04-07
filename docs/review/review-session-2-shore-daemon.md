# Session 2–3: shore-daemon Architecture Review

**Scope**: `shore-daemon` crate — the central service binary.
**Date**: 2026-04-06
**Prior session**: `docs/review-session-1-foundation-crates.md`

---

## Overall Assessment

`shore-daemon` is a surprisingly cohesive single-binary service for its complexity. The layering is sound: SWP server → message handler → engine → LLM client. The code is well-organized, consistently structured, and shows clear iterative refinement. That said, there are structural issues that a senior reviewer would flag — some correctness risks, some performance cliffs, and several areas where the architecture will resist future changes.

---

## 1. Critical: Race Conditions & Correctness Bugs

### 1.1 Autonomous message persistence bypasses the engine lock

`autonomy/manager.rs:1034-1043` — When the interiority system sends an autonomous message, it writes directly to `active.jsonl` via `std::fs::OpenOptions::append()` **without holding the engine lock**. Meanwhile, `persist_and_notify()` in `handler.rs:1038` holds the engine lock and does a full-file rewrite (`std::fs::write`).

**Scenario**: User sends a message → handler starts generation → interiority tick fires → autonomous message appends to `active.jsonl` → handler finishes generation, calls `persist()` → **full rewrite overwrites the autonomous message**.

This is a data loss bug. The fix requires autonomous messages to go through the engine's `append_message()` path, which means the tick loop needs access to the engine Arc (or a channel to serialize writes).

### 1.2 Double reload check for compaction

`handler.rs:452-456` checks `take_needs_reload` and reloads before appending the user message. Then `handler.rs:580-583` checks it **again** and reloads a second time. The first reload is correct (must happen before append to avoid overwriting compacted data). The second is redundant and wastes an I/O cycle — but more importantly, it means there's a window where the engine is reloaded without re-locking, and the messages vector was cloned from the pre-reload state at line 599, which would use stale data.

The ordering is confusing and fragile. The two reload checks should be consolidated into one.

### 1.3 LedgerStream not finalized on error paths

In `engine/tools.rs:237-243`, if `stream_raw` fails, the `LedgerStream` is dropped without `finalize()`. The stream's Drop impl presumably logs but doesn't record in the ledger. In `handler.rs:856-876`, the `stream_result` block correctly calls `finalize` on the Ok path but drops the `ledger_stream` on Err without finalizing.

This means failed API calls may not be recorded in the ledger, producing inaccurate cost tracking.

### 1.4 `rid` propagation gap

`handler.rs:433` — `rid: _` discards the request ID. It's used only in the `#[instrument]` macro for tracing. The `LlmClient` has an unused `_rid` parameter. This means request tracing is incomplete — you cannot correlate an LLM API call back to the originating client request ID through the ledger.

---

## 2. Performance Cliffs

### 2.1 MessageStore::persist() is O(n) full-file rewrite on every mutation

`engine/messages.rs:209-232` — Every `append()`, `edit()`, `delete()`, and `set_swipe()` rewrites the **entire** `active.jsonl` file. After a long conversation with hundreds of messages, each mutation becomes a multi-millisecond blocking I/O operation.

This is called while holding the engine's tokio Mutex, blocking any other task that needs the engine (including the handler trying to append the next message). For conversations with 500+ messages, this will cause noticeable latency.

**Recommendation**: Switch to append-only writes for `append()`, and only do full rewrites for `edit()`/`delete()`. Or buffer writes and batch them.

### 2.2 Blocking filesystem I/O on async tasks

`handler.rs` — `build_content()`, `build_llm_messages()`, `ingest_images()` all call `std::fs::read()` synchronously on the async task. For image-heavy conversations, this blocks the tokio runtime. The same applies to `MessageStore::persist()` which calls `std::fs::write()`.

The `MemoryDB` SQLite operations are also synchronous and run on async tasks without `spawn_blocking`.

### 2.3 Per-request MemoryDB and VectorStore construction

`handler.rs:929-962` — Every tool loop invocation opens a fresh `MemoryDB` (SQLite connection) and potentially a fresh `VectorStore` (LanceDB connection). For high-frequency tool use, this means repeated SQLite initialization and LanceDB table opens. These should be pooled or cached.

### 2.4 Activity tracker backfill reads all segments synchronously

`handler.rs:536-575` — On first autonomy state creation for a character, the handler reads **every message in every archived segment** to backfill the activity tracker. This happens synchronously while holding the engine lock. For characters with large histories, this blocks the engine for seconds.

---

## 3. Architectural Concerns

### 3.1 The handler.rs god file (1854 lines)

`handler.rs` contains generation orchestration, image ingestion, image embedding, tool context construction, streaming with retry, persistence, and notification. It has grown organically to encompass the entire message lifecycle. The phases (1-12) are documented in comments but not reflected in the module structure.

**Recommendation**: Split into `handler/mod.rs` (routing), `handler/generation.rs` (main generation flow), `handler/images.rs` (image handling), `handler/context.rs` (tool context construction). The `GenContext` and `GenerationParams` structs already define natural boundaries.

### 3.2 Two separate tool-loop implementations

The generation path (`engine/tools.rs`) and the interiority path (`autonomy/manager.rs:830-1013`) both implement tool loops with nearly identical logic:

1. Parse tool_uses from response
2. Dispatch tools
3. Build tool_result messages
4. Call LLM again
5. Repeat until max_iterations or non-tool_use finish_reason

The interiority version is subtly different (non-streaming `generate()` vs streaming, different content block handling, no diagnostics recording). Any bug fix or feature addition must be applied to both.

**Recommendation**: Extract a shared `ToolLoop` runner that both paths use, parameterized by streaming mode and diagnostics hooks.

### 3.3 AutonomyManager does too much

At 1541 lines, `AutonomyManager` manages:

- Per-character state persistence
- Interiority clock ticking
- Activity tracking
- Cache keepalive pings
- Compaction triggering
- **LLM calls** (the tick loop directly calls `client.generate()`)
- **Tool dispatch** (the tick loop directly calls `dispatch_tool()`)
- Message persistence (direct `active.jsonl` append)

It's simultaneously a state manager, a scheduler, an LLM caller, a tool executor, and a persistence layer. This makes it impossible to test in isolation.

### 3.4 No explicit abort of in-flight generation on client disconnect

When a client disconnects (Unix socket closed), there's no mechanism to abort the in-flight generation task. The `Cancel` message aborts it, but that requires the client to explicitly send Cancel before disconnecting. If the client crashes, the generation continues to completion, consuming API credits.

### 3.5 `dashmap` for autonomy states is overkill

`AutonomyManager::states` uses `DashMap<String, Arc<Mutex<AutonomyState>>>`. The `DashMap` provides concurrent access, but every access immediately locks the `Mutex` inside. The `DashMap` only protects the map structure itself (insert/remove), not the state access pattern. A `std::sync::Mutex<HashMap<...>>` would be simpler and equally performant since all state access is already serialized through the inner Mutex.

---

## 4. Robustness & Resilience

### 4.1 Compaction has no rollback

`memory/compaction/background.rs` — If compaction creates memory entries and indexes vectors but then fails to write the compacted `active.jsonl`, the system is in an inconsistent state: memory entries exist for messages that are still in the active window. The next compaction may create duplicate entries.

### 4.2 Interiority tick interval is hardcoded

The interiority tick interval is configured in `AutonomyConfig` but the actual polling loop uses a fixed interval. This means the config knob is a no-op.

### 4.3 No graceful degradation for VectorStore failures

If LanceDB fails to open during a tool loop, the entire tool loop fails (`run_tool_phase` returns an error). Vector search should be a degraded capability, not a fatal one. The `search_ctx` is already `Option`, so it degrades gracefully when not configured — but not when it fails at runtime.

### 4.4 `broadcast` channel capacity of 256

`server/mod.rs:73` — The broadcast channel has capacity 256. If a client is slow to consume (e.g., TUI rendering a large message), it will miss messages. There's no backpressure mechanism or missed-message recovery.

---

## 5. Code Quality

### 5.1 Good patterns worth preserving

- **`ToolContext` trait with `Sync` bound**: Clean dependency injection for tools
- **Lazy engine creation in `CharacterRegistry`**: Engines are only created when first needed
- **Separate `LedgerClient` wrapping `LlmClient`**: Clean separation of billing from transport
- **`serialize_for_storage()` on Message**: Different serialization for disk vs wire
- **Per-character config merging**: Each character can override global config without affecting others

### 5.2 Inconsistencies

- `build_content()` in `handler.rs:183` uses `std::fs::read` for images but `build_llm_messages()` at line 781 does the same thing independently — two code paths that should be unified
- The `rid` field exists in `ClientMessageBody` but is discarded everywhere except tracing
- `ContentBlock` handling varies: some paths use `content_block_to_json`, others `content_block_to_api_json`, others manually construct `json!({...})` — no single canonical path

### 5.3 Test coverage

- Engine module has solid unit tests (messages, segments, engine CRUD)
- Tool loop has integration tests with mock SSE servers
- Tool dispatch has coverage for all registered names
- **Missing**: No tests for the generation pipeline (handler.rs), autonomy tick loop, or compaction
- **Missing**: No test for the autonomous message persistence race condition

---

## 6. Carry-Forward Question Answers (from Session 1)

1. **`unsafe impl Sync for MemoryDB`** — The single-task invariant **holds**. A fresh `MemoryDB` is opened per-request in `run_tool_phase`, per interiority tick in `build_tool_context`, and per compaction in `run_compaction`. No two concurrent tasks share a `MemoryDB` instance.

2. **`rid` propagation gap** — **Confirmed**: `rid` is extracted from `ClientMessage` but discarded (`rid: _`) in `handle_generation`. It is NOT passed to `LlmClient` calls. The `_rid` parameter in `shore-llm-client` remains unused. The only trace is in the `#[instrument]` macro.

3. **History push after state changes** — **Confirmed**: `ConversationEngine::append_message` calls `self.broadcast_history()` after every mutation, sending merged tool-loop messages via `ServerMessage::History`.

4. **NewMessage push for autonomous messages** — **Race condition identified**: Autonomous messages are written directly to `active.jsonl` via `std::fs::append` WITHOUT holding the engine lock (`autonomy/manager.rs:1036`). The engine's in-memory `MessageStore` diverges from disk until next reload.

5. **LedgerStream finalization contract** — On error paths (failed `stream_raw`), the `LedgerStream` is dropped without calling `finalize()`. The Drop guard logs but doesn't record. This is confirmed as a gap.

6. **`merge_tool_loop_messages()`** — The daemon stores raw, un-merged tool-loop messages in `active.jsonl`. Merge happens only when broadcasting `History`. Raw intermediates are also pushed as individual `ToolCall`/`ToolResult` events during streaming.

---

## 7. Priority Recommendations

| Priority | Issue | Impact |
|----------|-------|--------|
| **P0** | Autonomous message race condition (1.1) | Data loss |
| **P0** | Compaction rollback (4.1) | Data corruption |
| **P1** | MessageStore O(n) persist (2.1) | Latency cliff at scale |
| **P1** | Blocking I/O on async tasks (2.2) | Runtime stalls |
| **P1** | LedgerStream finalization gap (1.3) | Inaccurate cost tracking |
| **P2** | Duplicate tool-loop code (3.2) | Maintenance burden |
| **P2** | Handler god file (3.1) | Cognitive load |
| **P2** | Per-request DB construction (2.3) | Unnecessary overhead |
| **P3** | Interiority hardcoded interval (4.2) | Config knob is misleading |
| **P3** | rid propagation (1.4) | Incomplete tracing |
| **P3** | DashMap overkill (3.5) | Unnecessary complexity |

---

## 8. Structural Inventory (for reference)

### Module map

```
shore-daemon/src/
├── main.rs              (316 lines) — startup, shutdown, compaction task
├── lib.rs               (15 lines)  — module declarations
├── handler.rs           (1854 lines) — message processing orchestrator
├── server/
│   ├── mod.rs           (1034 lines) — SWP server, client handling
│   └── registry.rs      — instance registry
├── engine/
│   ├── mod.rs           (345 lines)  — ConversationEngine
│   ├── messages.rs      (585 lines)  — MessageStore (JSONL persistence)
│   ├── segments.rs      — SegmentReader (frozen segments)
│   ├── prompt.rs        (1507 lines) — prompt assembly
│   └── tools.rs         (736 lines)  — tool loop
├── memory/
│   ├── mod.rs           — module declarations
│   ├── db.rs            (1376+ lines) — MemoryDB SQLite
│   ├── agent/           — MemoryAgent
│   ├── agent_llm.rs     — AgentLlm trait
│   ├── compaction/      — CompactionManager (1009 lines)
│   ├── collation/       — collation subsystem
│   ├── vectorstore.rs   — LanceDB vector store
│   ├── rag.rs           — legacy RAG
│   ├── search.rs        — hybrid search
│   └── researcher.rs    — deep-dive research agent
├── autonomy/
│   ├── mod.rs           — types, event log
│   ├── manager.rs       (1541 lines) — AutonomyManager
│   ├── interiority.rs   (661 lines)  — InteriorityClock
│   └── activity.rs      (811 lines)  — ActivityTracker
├── tools/
│   ├── mod.rs           (405 lines)  — tool registry and dispatch
│   ├── context.rs       — SharedToolContext, NoopRag
│   ├── basic.rs         — check_time, roll_dice
│   ├── memory_tools.rs  — memory tool handler
│   ├── images.rs        — image tools
│   ├── web.rs           — web tools
│   ├── scratchpad.rs    — scratchpad tools
│   └── activity.rs      — activity heatmap tool
├── commands/
│   ├── mod.rs           (376 lines)  — dispatch
│   ├── state.rs         — state commands
│   ├── conversation.rs  — conversation commands
│   ├── navigation.rs    — navigation commands
│   └── usage.rs         — usage commands
├── characters.rs        (383 lines)  — CharacterRegistry
├── notifications.rs     (213 lines)  — NotificationService
├── content_util.rs      (295 lines)  — ContentBlock JSON conversion
├── compat.rs            — backwards compat
├── templates.rs         — template utilities
└── test_support.rs      — test utilities
```

### Core data flow

```
Client → SWP Server → MessageHandler
  ├── Command → CommandContext → commands::dispatch → ServerMessage
  └── Message/Regen → spawned tokio task:
        1. Acquire engine (brief registry lock)
        2. Reload if compaction occurred
        3. Append/truncate user message
        4. Resolve model
        5. Ensure autonomy state
        6. Load character/user definitions
        7. Clone messages from engine
        8. Assemble prompt (token budget trimming)
        9. Build LLM request
       10. Stream with retry → StreamConsumer
       11. Tool loop (if tool_use)
       12. Persist + broadcast + notify
```

### Concurrency model

- **Single handler loop** processes routed messages sequentially
- **Generation tasks** are spawned as independent tokio tasks (one at a time, tracked via `generation_handle`)
- **Per-character interiority tasks** run independently on fixed intervals
- **Compaction task** runs in a dedicated background task driven by a channel
- **Engine access** is serialized via `Arc<Mutex<ConversationEngine>>` per character
- **Registry access** is serialized via `Arc<Mutex<CharacterRegistry>>` (single handler lock)
