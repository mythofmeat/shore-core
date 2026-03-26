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

- 9.1 **Compact command** — WIRING
  Library: CompactionManager (working). Daemon compact command is a stub.
  Needs production implementations of CompactionLlm, VectorIndexer,
  ConversationManager traits.

- 9.4 **Compaction trigger** — WIRING
  Library: Compactor with idle timer (working).
  Daemon: No integration with engine activity signal.

- 9.6 **Collation trigger** — WIRING
  Library: 4-phase pipeline (working).
  Daemon: No integration with engine or CLI.

- 5.15 **Manual compaction** — WIRING
  CLI sends command; daemon handler is a stub. Compactor library code exists.


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
- 5.3 **Stdin/pipe support for send** — MISSING
  `echo "hello" | shore send` opens $EDITOR instead of reading stdin.
  Blocks common Unix composition patterns (piping, heredocs).
- 5.7 Log follow mode (-f/--follow) — MISSING
- 5.8 Log format options (--json/--heartbeat/--content) — MISSING

### Conversation Management
- 5.12 Fork conversation (fork last N messages) — MISSING
- 5.13 Search conversations (full-text) — MISSING
- 5.14 Conversation info — MISSING

### Message CRUD
- 5.17 **Relative message references for edit/delete** — MISSING
  edit and delete require opaque m_<uuid> IDs. Need support for `last`, `-1`,
  or numeric index so users don't have to dig through JSON log output.
- 5.18 Get message by index — MISSING
- 5.19 Insert message at position — MISSING
- 5.20 Detach attachment — MISSING

### Character Management
- 5.24 Create character (scaffold directory) — MISSING

### Model Management
- 5.28 Reset to default — MISSING

### Memory CLI
- 5.31 Memory collation (manual trigger) — MISSING
- 5.32 Memory reindex — MISSING
- 5.33 Memory import — MISSING
- 5.34 Memory ask (one-shot agent) — MISSING as CLI; engine-side agent works.
- 5.35 Memory shell (REPL) — STUB
- 5.36 Memory changelog — MISSING

### Configuration
- 5.37 **Config key/section mismatch** — BUG
  CLI sends `{"key": ..., "value": ...}` but daemon config handler reads
  `args.get("section")`. `shore config defaults` silently returns full config
  instead of the filtered section.
- 5.38 Config show (all sections) — MISSING
- 5.39 Config check (validation) — MISSING (load_config validates on startup)
- 5.40 Config reset (clear overrides) — MISSING

### Config Schema Gaps
Config fields that exist in V1 but have no V2 schema support yet.
Needs design work before implementing — some may not map 1:1.

- 10.1 **defaults.cli_target_character** — MISSING
  Default character to load on startup.
- 10.2 **defaults.display_name** — MISSING
  User's display name in conversations.
- 10.3 **Per-tool toggles** (send_image, roll_dice, image_generation, web_search) — MISSING
  V1 had per-tool enable/disable under [behavior.tool_use].
- 10.4 **connections.tcp** (enabled, addr, allowed_hosts) — MISSING
  V1 had TCP access control. V2 has daemon.tcp_addr but no ACL.
- 10.5 **connections.matrix_embedded** — MISSING
  Embedded Synapse config (server_name, admin credentials).
- 10.6 **memory.image.enabled** — MISSING
  Toggle for image memory subsystem.
- 10.7 **Autonomy sub-toggles** (heartbeat.enabled, compaction.enabled, collation.enabled) — MISSING
  V1 had per-subsystem enabled flags. V2 only has the top-level autonomy.enabled.
- 10.8 **compaction.message_trigger / min_new_messages** — MISSING
  V1 had message-count-based compaction triggers. V2 only has idle_trigger_minutes.
- 10.9 **advanced.editor** — MISSING
  Config-level editor preference. V2 reads $VISUAL/$EDITOR env vars only.
- 10.10 **advanced.data_dir** — MISSING
  Config-level data directory override. V2 uses XDG only.
- 10.11 **advanced.max_retries / retry_backoff_seconds** — MISSING
  Config-level retry tuning. V2 has hardcoded retry logic in LLM client.
- 10.12 **debug.anthropic_cache** (log_expected_misses, preflight_check, exit_on_unexpected_miss) — MISSING
  Cache debug instrumentation flags.


## Priority 5: Memory & Autonomy Extras

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

### CLI Output Formatting (UX audit, 2026-03-26)

- 7.10 **Human-readable command output** — MISSING
  All command responses (status, log, model, character --info, config) go through
  print_command_output which just pretty-prints JSON. Need human-formatted output
  for each command: log should render as chat transcript, status as dashboard,
  model list with `*` marker, etc.

- 7.11 **`--json` output mode flag** — MISSING
  Once human-readable formatting is the default, add `--json` flag for scripts.

- 7.12 **NO_COLOR / `--no-color` support** — MISSING
  No way to disable ANSI colors. Should respect the NO_COLOR env convention.

- 7.13 **Phase indicator before first token** — MISSING
  Phase messages (thinking, text_generation) arrive from daemon but are discarded
  in CLI streaming loop. Show dimmed "thinking..." to fill the TTFT gap.

- 7.14 **Tool result truncation** — MISSING
  Tool results are printed in full. Large tool outputs (search results, file
  contents) will flood the terminal. Should truncate with a length limit.

- 7.15 **Stream metadata abbreviation** — MISSING
  Metadata line uses full model ID (`claude-haiku-4-5-20251001`). Could use the
  short name from the catalog instead.


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


## Build & Packaging

- 11.1 **Binary name is `shore-cli`** — MISSING
  Cargo package name is `shore-cli` so the binary is `shore-cli`. Help text and
  clap are configured as `shore`. Need `[[bin]] name = "shore"` in Cargo.toml
  or a rename at install time so the installed binary is just `shore`.


## Platform Bridges

- 1.1 **Telegram bot** — MISSING
  Deferred per architecture doc. Message routing, typing indicators,
  image attachments, texting delay simulation.

- 1.2 **Discord bot** — MISSING
  Deferred per architecture doc. Slash commands, selective character filtering.
