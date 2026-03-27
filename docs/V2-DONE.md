# Shore V2 — Completed Features

Features that are fully implemented and working in the V2 (Rust/TypeScript) rewrite.

## Platform Bridges

- **Matrix bridge** — Synapse provisioning, E2EE, avatar sync, room binding. More capable than V1.

## Autonomy & Interiority

- **Heartbeat system** (5-state, social need, dormancy) — Library + daemon wired. Per-character tick tasks spawned on first message, event feeding from handler (user/assistant messages), state persisted to disk, configurable dormant threshold (default 1). Action execution fully wired: probe LLM calls, deferred message generation + conversation append, social need messages.
- **Cache keepalive** (Anthropic TTL refresh) — Library + daemon wired. Per-character tick tasks with idle detection, config derived from resolved model. Ping execution: clones last LlmRequest with max_tokens=1, feeds cache_read_tokens back to scheduler, pushes CacheWarning events to connected clients.
- **Autonomy state persistence** — Heartbeat state + cache keepalive counters saved to `{data_dir}/{character}/autonomy_state.json`. Restored on daemon restart with edge-case handling (expired deferrals, stale probes).
- **Auto-compaction** (idle trigger + max-messages) — AutonomyManager per-character tick tasks with idle timer and max-messages trigger. Background compaction task consumes channel and runs full pipeline. Activity notifications from handler reset timers.
- **Social need gated checks** — Social need rolls gated to ~30min intervals (±50% jitter) instead of every tick. Cumulative miss probability tracked for social need bar display.

## shore-llm Endpoints

- **Embedding endpoint** (3.15) — `POST /v1/embed` routes to `openai.embed()`. Daemon consumes via `LlmClient.embed()`. Wired into RAG pipeline via `RealVectorIndexer`.

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
- **Memory changelog / audit trail** — Changelog table exists in schema, agent writes to it. CLI command: `shore memory-changelog`.

## Tool Use

- **Unified tool system** — `dispatch_tool()` + `available_tools()` with privacy filtering (ToolCategory). Replaced legacy ToolRegistry.
- **Memory tool** (unified NL search/create/update) — Wired into engine tool dispatch. Routes through MemoryResearcher (if tool_model configured) or direct MemoryAgent.ask().
- **generate_image** (4.5) — `LlmClient.image_generate()` → shore-llm, download + save, memory entry creation.
- **web_search** (4.6) — Tavily Search API + synthesis. Configurable under `[behavior.tool_use.search]`.
- **fetch_url** (4.7) — reqwest + HTML stripping for readable text extraction.
- **send_image**
- **list_images** (semantic search)
- **recall_image**
- **roll_dice** — Full dice notation parser (NdS+/-M).
- **check_time** — Returns ISO 8601 datetime.
- **Tool loop cap** — Configurable max_iterations (default 10).
- **activity_heatmap** (2.7) — Real data from ActivityTracker via ToolContext. Returns hour_histogram (normalized densities), classifications (peak/trough/normal), engagement_score, sessions_per_day. Graceful empty fallback.

## CLI Commands

- **Memory status** (shore memory) — Returns entry/entity counts from MemoryDB. Handles missing DB gracefully.
- **Memory query** (shore memory "query") — One-shot MemoryAgent query via CLI. Resolves agent model from config, uses CallerIdentity::User for pronoun resolution.
- **Memory reindex** (shore memory --reindex) — Rebuilds FTS5 and LanceDB vector indexes from all active entries. Batch embeds via LlmClient.
- **Send message** (shore send) — Supports `-i`/`--image` flag for multi-image attachments.
- **In-context image description** — handler.rs builds Anthropic content arrays with base64-encoded images. Media type detection by extension (jpg, png, gif, webp).
- **Regenerate** (shore regen [--guidance])
- **Log** (--count flag, -f/--follow mode, --json/--content format options)
- **Edit message**
- **Get message by index** (`shore get <ref>`)
- **Delete message** (supports multiple refs)
- **List characters** (scans config/characters directory)
- **Create character** (`shore character --new <name>`, scaffolds directory)
- **Switch character** (creates new engine instance, client-side state file)
- **List models**
- **Switch model** (accepts short or qualified names)
- **Model reset** (`shore model --reset`, revert to default)
- **Config get** (shore config / shore config <section>)
- **Config set** (shore config <key> <value>) — Runtime config changes with focused whitelist: defaults.model, defaults.stream, autonomy.enabled, cache_keepalive.enabled.
- **Config reset** (shore config --reset) — Reloads config from disk, clears runtime overrides.
- **Config path** (shore config --path) — Prints config directory, no daemon needed.
- **Status** (character, conversation, model, autonomy state/tau/keepalive, token counts)
- **Status sections** (`shore status --section <name>`, filtered view)
- **Completions** (fish, bash, zsh)
- **Send via editor** (shore send with no args opens $EDITOR)
- **Model info** (shore model <name> --info) — Full ResolvedModel details.
- **Character info** (shore character <name> --info) — Definition preview, user.md, prompt overrides.
- **Compact** (shore memory compact) — Full compaction + collation pipeline via daemon command.
- **Stdin/pipe support** (echo "hi" | shore send) — Reads stdin when not a terminal.
- **Relative message refs** (shore log edit last, shore log delete -1) — Supports `last`, negative indices, positive indices.
- **CLI restructuring** — Reduced from 16 to 9 user-facing commands. `get`/`edit`/`delete` folded under `log` as subcommands. `compact`/`collate`/`memory-changelog`/`--reindex` folded under `memory` as subcommands. `diagnostics` folded into `status --diagnostics`. Collation merged into compact pipeline (always runs after compaction).

## Configuration & Architecture

- **Model roles** (primary/tool/embedding/image) — DefaultsConfig has model, tool_model, memory_agent, embedding, image_generation slots.
- **Hierarchical config** — Nested [chat.provider.model] with provider defaults cascading into models. Unified config.toml replaces separate models.toml.
- **include/conf.d** — `include = [...]` for explicit file includes, `conf.d/*.toml` for automatic drop-in merging.
- **Per-model cache config** (ttl, depth, keepalive) — All cache fields are per-model in ResolvedModel.
- **Multi-provider reasoning effort** — reasoning_effort is a per-model field.
- **TCP / remote daemon access** — Config [connections.tcp] with enabled, addr, allowed_hosts ACL. SHORE_TCP_ADDR env var fallback. Replaces old [daemon].tcp_addr.
- **Thin-client mode** (no local config) — CLI --socket flag can point to remote.
- **Instance registry** — instances.json with file locking, register/unregister/list.
- **Runtime config overrides** — Model switch, per-character overrides, and general `config set` pathway (5.41) with focused whitelist.
- **Config auto-sync** (fills missing fields on startup)
- **Per-character config overrides** — Character definitions, user definitions, prompt templates all resolve per-character.
- **Process supervision** (shore-llm) — Daemon spawns and supervises shore-llm. Health checks, restart with backoff, SIGTERM/SIGKILL.
- **Per-tool toggles** (10.3) — Named bools under [behavior.tool_use.tools] for each of the 11 tools. Filtered in available_tools().
- **TCP access control** (10.4) — [connections.tcp] with enabled, addr, allowed_hosts. ACL enforced on TCP accept (empty = allow all).
- **memory.image_enabled** (10.6) — Toggle for image memory subsystem under [memory].
- **Autonomy sub-toggles** (10.7) — heartbeat.enabled, compaction.enabled, collation.enabled under their respective [behavior.autonomy.*] sections. Top-level autonomy.enabled still gates everything.
- **Compaction message triggers** (10.8) — Already covered: min_messages (idle gate) + max_messages (force trigger) in CompactionConfig.
- **advanced.editor** (10.9) — Config-level editor override, checked before $VISUAL/$EDITOR.
- **advanced.data_dir** (10.10) — Decision: use $XDG_DATA_HOME env var. No config field needed.
- **advanced.max_retries / retry_backoff_seconds** (10.11) — Config-level retry tuning. Wired into RetryPolicy and exponential backoff in handler.

## Message Storage

- **Persist tool calls and reasoning in messages** (2.9) — Expanded Message with `content_blocks: Vec<ContentBlock>` (Text, Thinking, ToolUse, ToolResult). Tool loop intermediate messages persisted to JSONL. Payload rebuilt from content_blocks. Old conversations load fine via serde defaults.

## Rendering & UX

- **Streaming responses** — With thinking token support.
- **TUI** — Full terminal UI with vim-style keybindings, image display (Kitty/iTerm2), markdown rendering.
- **Human-readable `log` output** — Colored chat transcript with section headers, timestamps, image badges, character-colored names via deterministic hash.
- **Human-readable `status` output** — Dashboard with character/model/messages, autonomy heartbeat state (plain English descriptions), social need bar (cumulative probability), roll probability, cache keepalive info. Conditional sections hide when data is absent.
- **`NO_COLOR` / `--no-color` support** — Respects NO_COLOR env var and --no-color CLI flag via global AtomicBool.
- **Phase indicator before first token** — Shows generation phase during streaming.
- **Tool result truncation** — 500 char limit in CLI display.
- **Stream metadata abbreviation** — Strips date suffix from model names.
- **Verbose spinner** (7.6) — `StreamSpinner` shows elapsed time and current phase during streaming, updated every 200ms. Clears on first content chunk. Non-terminal safe (no-op when piped).
- **Human-readable command output** (7.10) — All CLI commands now produce formatted, colored output instead of raw JSON. `format_command()` dispatcher routes to per-command formatters: `model` (table with active indicator), `model --info` (key-value details), `character --info` (definition preview), `memory` (status counts), `memory changelog` (timestamped operation table), `memory compact` (compaction + collation summary), `config` (recursive key-value display), `config --check` (validation with warnings/info), `status --diagnostics` (API calls, tool calls, errors tables), `log <ref>` (single message transcript), `log edit`/`log delete` (confirmations).
- **`--json` output mode flag** (7.11) — All commands with rich output support `--json` for script consumption: `log`, `status`, `model`, `character`, `memory`, `config`. Generic `recv_command_response` replaced by `recv_command_data` + per-command formatting/JSON dispatch.

## Memory Maintenance

- **Consolidation** (write-time dedup via LLM) — Handled by collation merge phase (Phase 2: cluster + deduplicate) and memory agent create/supersede flow. No additional dedup mechanism needed.

## Rendering (additional)

- **Rich markdown rendering** — Custom parser in shore-tui/src/markdown.rs. Covers bold, italic, inline code, code blocks, headings, blockquotes. Not full CommonMark but sufficient for chat display.

## Push Notifications

- **Push notifications** (5.44) — Daemon-side notification service with 3 backends: `notify_send` (Linux desktop), `ntfy` (mobile push via ntfy.sh or self-hosted), `command` (user-defined shell template). Per-event toggles for autonomous messages, cache warnings, compaction/collation completion, and errors. Fire-and-forget dispatch via tokio::spawn, best-effort delivery. Config under `[notifications]`.

## Observability

- **Structured JSON logging** — tracing + tracing-subscriber with JSON output, env filter, thread IDs.
- **API payload logging** (8.2) — `advanced.api_payload_logging` config flag. Logs request payloads to `{data_dir}/api_payloads.jsonl` with API keys redacted. Covers streaming and non-streaming requests.
- **Cache debug guards** (8.3) — 5-layer guard in `check_cache_invalidation()`: checks warnings enabled, cache_read_tokens==0, turn count >1, not first after restart/compaction. Pushes `CacheWarning` to connected clients. 5 unit tests.
- **shore-llm lifecycle robustness** (8.5) — Startup socket check warns when shore-llm is externally managed and socket is missing. Actionable error messages by error kind (NotFound, ConnectionRefused, PermissionDenied).
- **In-memory ring buffers** (8.1) — `Diagnostics` struct with `RingBuffer<T>` (capacity 100) for API calls, tool executions, and errors. Recorded in handler and tool loop. Exposed via `shore diagnostics [-n count]` command.
