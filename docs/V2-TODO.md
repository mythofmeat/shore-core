# Shore V2 — Remaining Work

Features that still need implementation or wiring to reach V1 parity.

Status key:
- STUB = code exists but returns placeholder/error
- WIRING = both library and daemon exist but aren't connected
- MISSING = no code at all


## Priority 1: Wiring Gaps

Both the subsystem library code AND the daemon core exist for these,
but they aren't connected to each other yet. These are the lowest-hanging
fruit for getting the system functional.

- 9.1 **Memory commands** — WIRING
  Library: MemoryDB + memory agent (working).
  Daemon: memory/compact commands return "not_implemented".

- 9.2 **RAG in prompt assembly** — WIRING
  Library: RAG pipeline + vector store (working).
  Daemon: engine/prompt.rs RAG injection returns None.

- 9.3 **Memory tools in tool loop** — WIRING
  Library: Tool handlers (working).
  Daemon: engine/tools.rs has built-in tools but not memory/image/web tools.

- 9.4 **Compaction trigger** — WIRING
  Library: Compactor with idle timer (working).
  Daemon: No integration with engine activity signal.

- 9.5 **Heartbeat in event loop** — WIRING
  Library: HeartbeatScheduler (working).
  Daemon: Not spawned by main.rs.

- 9.6 **Collation trigger** — WIRING
  Library: 4-phase pipeline (working).
  Daemon: No integration with engine or CLI.

- 9.7 **Cache keepalive in event loop** — WIRING
  Library: CacheKeepaliveScheduler (working).
  Daemon: Not spawned by main.rs.

- 5.15 **Manual compaction** — WIRING
  CLI sends command; daemon handler is a stub. Compactor library code exists.

- 5.29 **Memory query** — WIRING
  CLI sends command; daemon handler is a stub. Library memory agent works.

- 5.30 **Memory status** — WIRING
  Library has entry counts, daemon doesn't expose them.


## Priority 2: shore-llm Endpoints

These depend on shore-llm implementing the endpoints.

- 3.15 **Embedding endpoint** — STUB
  shore-llm /v1/embed returns 501. Needed for RAG vector search.

- 4.5 **generate_image** — STUB
  shore-llm /v1/image/generate returns 501.


## Priority 3: Tool Use

- 4.6 **web_search** (Tavily API + synthesis) — STUB
  Returns NotImplemented. Needs Tavily integration in daemon.

- 4.7 **fetch_url** (readable text extraction) — STUB
  Returns NotImplemented. Needs HTTP client + readability extraction.

- 4.8 **research_web** (multi-step deep research) — STUB
  Returns NotImplemented. Depends on 4.6 + 4.7.

- 2.7 **Activity heatmap engine** — STUB
  Tool returns placeholder JSON. Heatmap data collection not implemented.


## Priority 4: CLI Features

### Messaging
- 5.2 Send with image attachment (-i flag) — MISSING
- 5.3 Send via editor (no args opens $EDITOR) — MISSING
- 5.7 Log follow mode (-f/--follow) — MISSING
- 5.8 Log format options (--json/--heartbeat/--content) — MISSING

### Conversation Management
- 5.12 Fork conversation (fork last N messages) — MISSING
- 5.13 Search conversations (full-text) — MISSING
- 5.14 Conversation info — MISSING

### Message CRUD
- 5.18 Get message by index — MISSING
- 5.19 Insert message at position — MISSING
- 5.20 Detach attachment — MISSING

### Character Management
- 5.23 Character info — MISSING
- 5.24 Create character (scaffold directory) — MISSING

### Model Management
- 5.27 Model info — MISSING
- 5.28 Reset to default — MISSING

### Memory CLI
- 5.31 Memory collation (manual trigger) — MISSING
- 5.32 Memory reindex — MISSING
- 5.33 Memory import — MISSING
- 5.34 Memory ask (one-shot agent) — MISSING as CLI; engine-side agent works.
- 5.35 Memory shell (REPL) — STUB
- 5.36 Memory changelog — MISSING

### Configuration
- 5.38 Config show (all sections) — MISSING
- 5.39 Config check (validation) — MISSING (load_config validates on startup)
- 5.40 Config reset (clear overrides) — MISSING
- 5.41 Config path — MISSING


## Priority 5: Autonomy & Interiority

- 2.4 **Interiority — journal writing** — MISSING
- 2.5 **Interiority — story writing** — MISSING
- 2.6 **Interiority scheduling** (adaptive timing, pause/resume) — MISSING
- 2.8 **Autonomy pause/resume** — MISSING
  V2 only has toggle-autonomy (on/off). No temporary pause with auto-resume.

- 3.5 **Consolidation** (write-time dedup via LLM) — UNKNOWN
  Needs verification — may be handled by memory agent create/supersede flow.

- 3.14 **Memory import** (files → entries) — MISSING
- 3.12 **Memory agent — interactive REPL** — STUB


## Priority 6: Rendering & UX

- 7.2 Inline terminal images, Kitty/Ghostty (APC protocol) — MISSING
- 7.3 Inline terminal images, iTerm2 (OSC 1337) — MISSING
- 7.4 $SHORE_IMAGES override — MISSING
- 7.5 Rich markdown rendering — UNKNOWN (V2 renders streamed text, quality unverified)
- 7.6 Verbose spinner (token counts, cache hits, timing) — MISSING


## Priority 7: Observability

- 8.1 In-memory ring buffers (API calls, tools, errors) — MISSING
- 8.2 API payload logging (api_payloads.jsonl) — MISSING
- 8.3 Cache debug guards — MISSING (config has cache_invalidation_warnings bool)
- 8.4 Status sections (filtered view) — MISSING


## Priority 8: Other CLI

- 5.44 Push notifications (shore notify) — MISSING
- 5.45 Failed message list — MISSING
- 5.46 Failed message retry — MISSING
- 5.47 Failed message clear — MISSING
- 5.48 Cache suppress — MISSING
- 5.49 Cache unsuppress — MISSING
- 5.50 Images list (CLI-level browsing) — MISSING
- 5.51 Images import — MISSING
- 5.52 Images describe (vision model) — MISSING


## Platform Bridges

- 1.1 **Telegram bot** — MISSING
  Deferred per architecture doc. Message routing, typing indicators,
  image attachments, texting delay simulation.

- 1.2 **Discord bot** — MISSING
  Deferred per architecture doc. Slash commands, selective character filtering.
