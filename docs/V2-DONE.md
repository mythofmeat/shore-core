# Shore V2 — Completed Features

Features that are fully implemented and working in the V2 (Rust/TypeScript) rewrite.

## Platform Bridges

- **Matrix bridge** — Synapse provisioning, E2EE, avatar sync, room binding. More capable than V1.

## Autonomy & Interiority

- **Heartbeat system** (5-state, social need, dormancy) — Library + daemon wired. Per-character tick tasks spawned on first message, event feeding from handler (user/assistant messages), state persisted to disk, configurable dormant threshold (default 1). Action execution (LLM probe calls) not yet wired.
- **Cache keepalive** (Anthropic TTL refresh) — Library + daemon wired. Per-character tick tasks with idle detection, config derived from resolved model. Ping execution (minimal API calls) not yet wired.
- **Autonomy state persistence** — Heartbeat state + cache keepalive counters saved to `{data_dir}/{character}/autonomy_state.json`. Restored on daemon restart with edge-case handling (expired deferrals, stale probes).
- **Auto-compaction** (idle trigger + max-messages) — AutonomyManager per-character tick tasks with idle timer and max-messages trigger. Background compaction task consumes channel and runs full pipeline. Activity notifications from handler reset timers.

## Memory System

- **SQLite storage** (WAL mode) + **FTS5 full-text search** (porter stemming, relevance ranking)
- **LanceDB vector store**
- **RAG retrieval** (vector + BM25 + deranking) — Library complete. Used by image tools for semantic search.
- **Compaction** (conversation → memory) — Full pipeline: RealCompactionLlm, RealVectorIndexer, RealConversationManager. Daemon command handler + background auto-trigger. Archives to segments, writes recap, reloads engine.
- **Collation** (memory refinement) — 4-phase pipeline fully wired. RealCollationLlm (JSON parsing with markdown fence stripping). Auto-runs after compaction when `collation.auto_run = true`. Manual trigger via `shore collate`.
  - **Tidy phase** (split multi-topic entries)
  - **Merge phase** (cluster + deduplicate)
  - **Entity normalization**
  - **Confidence decay**
- **Entity registry** (case-insensitive, descriptions)
- **Memory agent — agentic LLM loop** — 9 inner tools (search_entries, query_db, create_entry, update_entry, supersede_entry, update_entity, merge_entity, create_flag, resolve_flag), max 40 iterations, read/write classification with confirmation flow. Matches V1 `_run_agent_loop()`. CallerIdentity resolves pronoun ambiguity.
- **Memory researcher** — Cheap-model tier (defaults.tool_model) with `ask_memory_agent` tool, max 15 iterations, refusal fallback. Matches V1 `MemoryResearcher`.
- **AgentLlm abstraction** — Trait + RealAgentLlm (production, via LlmClient) + MockAgentLlm (tests). Decouples agent loop from transport.
- **Memory changelog / audit trail** — Changelog table exists in schema, agent writes to it. No CLI command to read it.

## Tool Use

- **Unified tool system** — `dispatch_tool()` + `available_tools()` with privacy filtering (ToolCategory). Replaced legacy ToolRegistry.
- **Memory tool** (unified NL search/create/update) — Wired into engine tool dispatch. Routes through MemoryResearcher (if tool_model configured) or direct MemoryAgent.ask().
- **send_image**
- **list_images** (semantic search)
- **recall_image**
- **roll_dice** — Full dice notation parser (NdS+/-M).
- **check_time** — Returns ISO 8601 datetime.
- **Tool loop cap** — Configurable max_iterations (default 10).

## CLI Commands

- **Memory status** (shore memory) — Returns entry/entity counts from MemoryDB. Handles missing DB gracefully.
- **Memory query** (shore memory "query") — One-shot MemoryAgent query via CLI. Resolves agent model from config, uses CallerIdentity::User for pronoun resolution.
- **Send message** (shore send)
- **Regenerate** (shore regen [--guidance])
- **Log** (--count flag)
- **Edit message**
- **Delete message** (supports multiple refs)
- **List characters** (scans config/characters directory)
- **Switch character** (creates new engine instance, client-side state file)
- **List models**
- **Switch model** (accepts short or qualified names)
- **Config get** (shore config / shore config <section>)
- **Config path** (shore config --path) — Prints config directory, no daemon needed.
- **Status** (character, conversation, model, autonomy state/tau/keepalive, token counts)
- **Completions** (fish, bash, zsh)
- **Send via editor** (shore send with no args opens $EDITOR)
- **Model info** (shore model <name> --info) — Full ResolvedModel details.
- **Character info** (shore character <name> --info) — Definition preview, user.md, prompt overrides.
- **Compact** (shore compact) — Full compaction pipeline via daemon command.
- **Collate** (shore collate) — Manual collation trigger via daemon command.
- **Stdin/pipe support** (echo "hi" | shore send) — Reads stdin when not a terminal.
- **Relative message refs** (shore edit last, shore delete -1) — Supports `last`, negative indices, positive indices.

## Configuration & Architecture

- **Model roles** (primary/tool/embedding/image) — DefaultsConfig has model, tool_model, memory_agent, embedding, image_generation slots.
- **Hierarchical config** — Nested [chat.provider.model] with provider defaults cascading into models. Unified config.toml replaces separate models.toml.
- **include/conf.d** — `include = [...]` for explicit file includes, `conf.d/*.toml` for automatic drop-in merging.
- **Per-model cache config** (ttl, depth, keepalive) — All cache fields are per-model in ResolvedModel.
- **Multi-provider reasoning effort** — reasoning_effort is a per-model field.
- **TCP / remote daemon access** — Config [daemon].tcp_addr + SHORE_TCP_ADDR env var.
- **Thin-client mode** (no local config) — CLI --socket flag can point to remote.
- **Instance registry** — instances.json with file locking, register/unregister/list.
- **Runtime config overrides** (model switch, per-character overrides — but no general `config set` pathway yet)
- **Config auto-sync** (fills missing fields on startup)
- **Per-character config overrides** — Character definitions, user definitions, prompt templates all resolve per-character.
- **Process supervision** (shore-llm) — Daemon spawns and supervises shore-llm. Health checks, restart with backoff, SIGTERM/SIGKILL.

## Rendering & UX

- **Streaming responses** — With thinking token support.
- **TUI** — Full terminal UI with vim-style keybindings, image display (Kitty/iTerm2), markdown rendering.

## Observability

- **Structured JSON logging** — tracing + tracing-subscriber with JSON output, env filter, thread IDs.
