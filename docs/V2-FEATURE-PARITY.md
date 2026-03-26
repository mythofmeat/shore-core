# Shore V2 Feature Parity Tracker

Status of V1 (Python) features in the V2 (Rust/TypeScript) rewrite.
Updated after merging the daemon core (US-010–018).

Status key:
- DONE = fully implemented
- STUB = code exists but returns placeholder/error
- WIRING = both sides exist but aren't connected yet
- MISSING = no code at all


## 1. Platform Bridges

- 1.1 Telegram bot — MISSING
  Deferred per architecture doc. Message routing, typing indicators,
  image attachments, texting delay simulation.

- 1.2 Discord bot — MISSING
  Deferred per architecture doc. Slash commands, selective character filtering.

- 1.3 Matrix bridge — DONE
  V2 has Synapse provisioning, E2EE, avatar sync, room binding.
  More capable than V1.


## 2. Autonomy & Interiority

- 2.1 Heartbeat system (5-state, social need, dormancy) — DONE
  Library code complete; needs wiring into engine event loop.

- 2.2 Cache keepalive (Anthropic TTL refresh) — DONE
  Full state machine with pause/resume, config hot-reload.

- 2.3 Auto-compaction (idle trigger + reactive fallback) — DONE
  Compactor with idle timer; needs wiring to engine activity signal.

- 2.4 Interiority — journal writing — MISSING
  No interiority subsystem in V2.

- 2.5 Interiority — story writing — MISSING

- 2.6 Interiority scheduling (adaptive timing, pause/resume) — MISSING

- 2.7 Activity heatmap engine — STUB
  Tool returns placeholder JSON. Heatmap data collection not implemented.

- 2.8 Autonomy pause/resume — MISSING
  V2 only has toggle-autonomy (on/off). No temporary pause with auto-resume.


## 3. Memory System

- 3.1 SQLite storage (WAL) — DONE
- 3.2 LanceDB vector store — DONE
- 3.3 RAG retrieval (vector + BM25 + deranking) — DONE
  Library code complete. Prompt assembly has RAG injection point but
  returns None — needs wiring.

- 3.4 Compaction (conversation -> memory) — DONE
  Library code complete. Daemon core compact command is a stub —
  needs wiring to MemoryDB.

- 3.5 Consolidation (write-time dedup via LLM) — UNKNOWN
  Needs verification — may be handled by memory agent create/supersede flow.

- 3.6 Collation — tidy phase (split multi-topic entries) — DONE
- 3.7 Collation — merge phase (cluster + deduplicate) — DONE
- 3.8 Collation — entity normalization — DONE
- 3.9 Collation — confidence decay — DONE
- 3.10 Entity registry (case-insensitive, descriptions) — DONE

- 3.11 Memory agent — one-shot query — DONE
  Pronoun resolution, RAG search, DB lookup.

- 3.12 Memory agent — interactive REPL (shore memory shell) — STUB
  Returns "not yet implemented."

- 3.13 Memory changelog / audit trail — WIRING
  Changelog table exists in schema, agent writes to it, but no CLI
  command to read it.

- 3.14 Memory import (files -> entries) — MISSING

- 3.15 Embedding endpoint — STUB
  shore-llm /v1/embed returns 501. Needed for RAG vector search.


## 4. Tool Use

- 4.1 Memory tool (unified NL search/create/update) — DONE
  Library handler exists; needs wiring into engine tool dispatch.

- 4.2 send_image — DONE
- 4.3 list_images (semantic search) — DONE
- 4.4 recall_image — DONE

- 4.5 generate_image — STUB
  shore-llm /v1/image/generate also returns 501.

- 4.6 web_search (Tavily API + synthesis) — STUB
  Returns NotImplemented. Needs Tavily integration in daemon.

- 4.7 fetch_url (readable text extraction) — STUB
  Returns NotImplemented. Needs HTTP client + readability extraction.

- 4.8 research_web (multi-step deep research) — STUB
  Returns NotImplemented. Depends on 4.6 + 4.7.

- 4.9 roll_dice — DONE
  Built into engine/tools.rs with full dice notation parser.

- 4.10 check_time — DONE
  New in V2 — built-in tool in engine/tools.rs.

- 4.11 Tool loop cap — DONE
  Configurable max_iterations (default 10). No wrap-up warning on
  penultimate iteration yet.


## 5. CLI Commands

### 5a. Messaging

- 5.1 Send message (shore-cli send) — DONE
- 5.2 Send with image attachment (-i flag) — MISSING
- 5.3 Send via editor (no args opens $EDITOR) — MISSING
- 5.4 Regenerate (shore-cli regen [--guidance]) — DONE
- 5.5 Swipe (prev/next/numeric index) — DONE
- 5.6 Log (--count flag only) — DONE
- 5.7 Log follow mode (-f/--follow) — MISSING
- 5.8 Log format options (--json/--heartbeat/--content) — MISSING

### 5b. Conversation Management

- 5.9 List conversations — DONE
- 5.10 New conversation — DONE
- 5.11 Switch conversation — DONE
- 5.12 Fork conversation (fork last N messages) — MISSING
- 5.13 Search conversations (full-text) — MISSING
- 5.14 Conversation info — MISSING
- 5.15 Manual compaction — WIRING
  CLI sends command; daemon handler is a stub. Compactor library code exists.

### 5c. Message CRUD

- 5.16 Edit message — DONE
- 5.17 Delete message (supports multiple refs) — DONE
- 5.18 Get message by index — MISSING
- 5.19 Insert message at position — MISSING
- 5.20 Detach attachment — MISSING

### 5d. Character Management

- 5.21 List characters — DONE (scans config/characters directory)
- 5.22 Switch character — DONE (creates new engine instance)
- 5.23 Character info — MISSING
- 5.24 Create character (scaffold directory) — MISSING

### 5e. Model Management

- 5.25 List models — DONE
- 5.26 Switch model — DONE
- 5.27 Model info — MISSING
- 5.28 Reset to default — MISSING

### 5f. Memory CLI

- 5.29 Memory query — WIRING
  CLI sends command; daemon handler is a stub. Library memory agent works.
- 5.30 Memory status — WIRING
  Library has entry counts, daemon doesn't expose them.
- 5.31 Memory collation (manual trigger) — MISSING
  Library collation pipeline works but no CLI/command trigger.
- 5.32 Memory reindex — MISSING
- 5.33 Memory import — MISSING
- 5.34 Memory ask (one-shot agent) — MISSING as CLI; engine-side agent works.
- 5.35 Memory shell (REPL) — STUB
- 5.36 Memory changelog — MISSING

### 5g. Configuration

- 5.37 Config get/set — DONE
- 5.38 Config show (all sections) — MISSING
- 5.39 Config check (validation) — MISSING (load_config validates on startup)
- 5.40 Config reset (clear overrides) — MISSING
- 5.41 Config path — MISSING

### 5h. Other CLI

- 5.42 Status — DONE (character, conversation, model, autonomy, token counts)
- 5.43 Completions — DONE
- 5.44 Push notifications (shore notify) — MISSING
- 5.45 Failed message list — MISSING
- 5.46 Failed message retry — MISSING
- 5.47 Failed message clear — MISSING
- 5.48 Cache suppress — MISSING
- 5.49 Cache unsuppress — MISSING
- 5.50 Images list (CLI-level browsing) — MISSING
- 5.51 Images import — MISSING
- 5.52 Images describe (vision model) — MISSING


## 6. Configuration & Architecture

- 6.1 Model roles (primary/tool/embedding/image) — MISSING
  V2 has provider defaults + flat model list but no role assignment.

- 6.2 Hierarchical models.toml — PARTIAL
  V2 has [[models]] array + [provider_defaults.<provider>] sections.
  Cascading works but format differs from V1's [chat.<provider>.<profile>].

- 6.3 Per-model cache config (ttl, depth, keepalive) — MISSING
  Cache keepalive exists but config is global, not per-model.

- 6.4 TCP / remote daemon access — DONE
  Config [daemon].tcp_addr + SHORE_TCP_ADDR env var.

- 6.5 Thin-client mode (no local config) — DONE
  CLI --socket flag can point to remote; no local config needed.

- 6.6 Instance registry — DONE
  instances.json with file locking, register/unregister/list.

- 6.7 Runtime config overrides — DONE
- 6.8 Config auto-sync (fills missing fields on startup) — DONE
- 6.9 Per-character config overrides — DONE
  Character definitions, user definitions, prompt templates all resolve per-character.

- 6.10 Multi-provider reasoning effort — MISSING
  V2 models.toml has no reasoning_effort field.

- 6.11 Process supervision (shore-llm) — DONE
  Daemon spawns and supervises shore-llm. Health checks, restart with
  backoff, SIGTERM/SIGKILL.


## 7. Rendering & UX

- 7.1 Streaming responses — DONE (with thinking token support)
- 7.2 Inline terminal images, Kitty/Ghostty (APC protocol) — MISSING
- 7.3 Inline terminal images, iTerm2 (OSC 1337) — MISSING
- 7.4 $SHORE_IMAGES override — MISSING
- 7.5 Rich markdown rendering — UNKNOWN (V2 renders streamed text, quality unverified)
- 7.6 Verbose spinner (token counts, cache hits, timing) — MISSING


## 8. Observability

- 8.1 In-memory ring buffers (API calls, tools, errors) — MISSING
- 8.2 API payload logging (api_payloads.jsonl) — MISSING
- 8.3 Cache debug guards — MISSING
  Config has cache_invalidation_warnings bool.
- 8.4 Status sections (filtered view) — MISSING
- 8.5 Structured JSON logging — DONE
  tracing + tracing-subscriber with JSON output, env filter, thread IDs.


## 9. Wiring Gaps

Both the subsystem library code AND the daemon core exist for these,
but they aren't connected to each other yet. The merge brought the
daemon core, but phases 3-7 built subsystems against mock trait
contexts, not the real engine.

- 9.1 Memory commands
  Library: MemoryDB + memory agent (working)
  Daemon: memory/compact commands return "not_implemented"

- 9.2 RAG in prompt assembly
  Library: RAG pipeline + vector store (working)
  Daemon: engine/prompt.rs RAG injection returns None

- 9.3 Memory tools in tool loop
  Library: Tool handlers (working)
  Daemon: engine/tools.rs has built-in tools but not memory/image/web tools

- 9.4 Compaction trigger
  Library: Compactor with idle timer (working)
  Daemon: No integration with engine activity signal

- 9.5 Heartbeat in event loop
  Library: HeartbeatScheduler (working)
  Daemon: Not spawned by main.rs

- 9.6 Collation trigger
  Library: 4-phase pipeline (working)
  Daemon: No integration with engine or CLI

- 9.7 Cache keepalive in event loop
  Library: CacheKeepaliveScheduler (working)
  Daemon: Not spawned by main.rs


## Summary

46 done, 6 stub, 12 wiring, 52 missing — 117 total items
