# Shore V2 — Completed Features

Features that are fully implemented and working in the V2 (Rust/TypeScript) rewrite.

## Platform Bridges

- **Matrix bridge** — Synapse provisioning, E2EE, avatar sync, room binding. More capable than V1.

## Autonomy & Interiority

- **Heartbeat system** (5-state, social need, dormancy) — Library complete; needs wiring into engine event loop.
- **Cache keepalive** (Anthropic TTL refresh) — Full state machine with pause/resume, config hot-reload.
- **Auto-compaction** (idle trigger + reactive fallback) — Compactor with idle timer; needs wiring to engine activity signal.

## Memory System

- **SQLite storage** (WAL mode)
- **LanceDB vector store**
- **RAG retrieval** (vector + BM25 + deranking) — Library complete. Prompt assembly has RAG injection point but returns None — needs wiring.
- **Compaction** (conversation → memory) — Library complete. Daemon compact command is a stub — needs wiring to MemoryDB.
- **Collation — tidy phase** (split multi-topic entries)
- **Collation — merge phase** (cluster + deduplicate)
- **Collation — entity normalization**
- **Collation — confidence decay**
- **Entity registry** (case-insensitive, descriptions)
- **Memory agent — one-shot query** — Pronoun resolution, RAG search, DB lookup.
- **Memory changelog / audit trail** — Changelog table exists in schema, agent writes to it. No CLI command to read it.

## Tool Use

- **Memory tool** (unified NL search/create/update) — Library handler exists; needs wiring into engine tool dispatch.
- **send_image**
- **list_images** (semantic search)
- **recall_image**
- **roll_dice** — Built into engine/tools.rs with full dice notation parser.
- **check_time** — New in V2 — built-in tool in engine/tools.rs.
- **Tool loop cap** — Configurable max_iterations (default 10).

## CLI Commands

- **Send message** (shore send)
- **Regenerate** (shore regen [--guidance])
- **Swipe** (prev/next/numeric index) — Daemon-side only; removed from CLI, will be TUI-only.
- **Log** (--count flag)
- **List conversations**
- **New conversation**
- **Switch conversation**
- **Edit message**
- **Delete message** (supports multiple refs)
- **List characters** (scans config/characters directory)
- **Switch character** (creates new engine instance, client-side state file)
- **List models**
- **Switch model** (accepts short or qualified names)
- **Config get/set**
- **Status** (character, conversation, model, autonomy, token counts)
- **Completions** (fish, bash, zsh)

## Configuration & Architecture

- **Model roles** (primary/tool/embedding/image) — DefaultsConfig has model, tool_model, memory_agent, embedding, image_generation slots.
- **Hierarchical config** — Nested [chat.provider.model] with provider defaults cascading into models. Unified config.toml replaces separate models.toml.
- **include/conf.d** — `include = [...]` for explicit file includes, `conf.d/*.toml` for automatic drop-in merging.
- **Per-model cache config** (ttl, depth, keepalive) — All cache fields are per-model in ResolvedModel.
- **Multi-provider reasoning effort** — reasoning_effort is a per-model field.
- **TCP / remote daemon access** — Config [daemon].tcp_addr + SHORE_TCP_ADDR env var.
- **Thin-client mode** (no local config) — CLI --socket flag can point to remote.
- **Instance registry** — instances.json with file locking, register/unregister/list.
- **Runtime config overrides**
- **Config auto-sync** (fills missing fields on startup)
- **Per-character config overrides** — Character definitions, user definitions, prompt templates all resolve per-character.
- **Process supervision** (shore-llm) — Daemon spawns and supervises shore-llm. Health checks, restart with backoff, SIGTERM/SIGKILL.

## Rendering & UX

- **Streaming responses** — With thinking token support.
- **TUI** — Full terminal UI with vim-style keybindings, image display (Kitty/iTerm2), markdown rendering.

## Observability

- **Structured JSON logging** — tracing + tracing-subscriber with JSON output, env filter, thread IDs.
