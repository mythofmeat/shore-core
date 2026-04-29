# Changelog

All notable changes to Shore are documented in this file.
Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] — OpenClawify

See `README.md`, `FEATURES.md`, and `CONFIGURATION.md` for current branch guidance.

### Added
- Effective model catalog (Phase 7): `shore model <name>` accepts manual aliases, upstream `<provider>/<id>` strings, and explicit `provider:model_id` selectors against a catalog merged from static config, provider discovery, and saved preferences
- Provider registry + discovery: `[providers.<name>]` tables with `[[keys]]` rotation, `discovery.enabled`, and gitignore-style `discovery.visibility` patterns; provider catalogs cache to `<data>/providers/<name>/models.json`
- CLI: `shore provider`, `shore provider models <name>`, `shore provider models <name> --all`, and `shore provider refresh <name>`
- CLI: `shore model setting [<key> [<value>]]` (with `--global`, `--reset`, and `--all`) for sampler preferences; `shore reasoning ...` keeps working and now writes through the same store
- TUI: `:provider`, `:provider refresh <name>`, `:model all`, and `:setting <key> <value>` slash commands; model picker tags rows with their source (`static`/`discovered`) and footer hints at hidden models
- Documentation: `examples/config.toml` ships an opt-in OpenRouter budget/overflow + discovery + visibility filter snippet; `CONFIGURATION.md` documents the effective-catalog merge order and the sampler-preferences precedence chain
- Time-aware prompt assembly: user messages now carry inline time markers when the gap from the previous turn is large, when an hour has elapsed since the last marker (so long, slow conversations stay anchored), or when prior context was lost to compaction (so the first user message after a cut still has an absolute date/time reference)
- Character workspaces with protected prompt files: `SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, and `HEARTBEAT.md`
- Markdown long-term memory under `characters/<Character>/workspace/memory/`
- `active_prompt/` snapshots and deferred protected self-edit activation
- Opt-in AI librarian dreaming with memory tools, `.dreams/`, `DREAMS.md`, and prompt-visible `MEMORY.md` indexing
- Optional `[defaults].dreaming` model selector for private memory librarian passes
- Dreaming now reuses the cached chat request prefix when available, and `MEMORY.md` index edits activate at compaction instead of immediately changing the hot prompt prefix
- Optional hybrid semantic+lexical markdown retrieval backed by a rebuildable index
- Workspace file tools and sandboxed `exec`
- Workspace `delete` tool that moves files into a timestamped trash directory under the character data dir; refuses prompt-visible files and directories
- Repository ownership layout: `core/`, `backend/`, `clients/`, `bridges/`, and `dev/`, with default Cargo members for faster local daemon/CLI builds

### Changed
- TUI keystroke latency no longer scales with conversation length: the conversation pane caches its rendered lines and rebuilds only when a fingerprint of rendering-relevant state changes
- Runtime memory source of truth moved from hidden SQLite/vector/RAG state to markdown files
- Interiority naming standardized to heartbeat/autonomy
- Compaction now writes markdown memory notes and activates staged protected edits without generating recap prompt files
- Docs rewritten against `GOALS.md` and current branch behavior
- Current docs now distinguish uploaded image vision from generated-image sending: uploaded attachment paths remain internal, while `generate_image` can create and send new images
- Internal crates renamed for clearer roles: `shore-swp-client`, `shore-swp-server`, and `shore-llm`

### Removed
- Runtime dependency on the old memory shell, collation pipeline, passive RAG injection, and authoritative vector memory store
- User-facing claims that `character.md`, `user.md`, or `prompts/system.md` are the active layout
- Stale current-docs references to removed scratchpad tools, removed standalone `memory_*` tools, and uploaded-attachment `send_image` behavior

## [0.15.0] — 2026-04-16

### Added
- `shore-mcp` crate: debug-only MCP server exposing the daemon over JSON-RPC stdio, with status, log, usage, character, model, memory, config, debug (heartbeat), send, regen, and log_follow tools
- `shore-mcp` profile resolution (main / persistent / ephemeral), daemon discover-or-spawn with `--daemon-addr` override, and write-op gating rules to prevent accidental writes to the main profile
- TTS integration: SWP message types for audio streaming, `[tts]` config section, daemon TTS client with WAV relay, `AudioPlayer` using rodio, `shore speak` CLI subcommand with on-demand and live modes, TUI `:speak` command with live-TTS toggle and indicator
- TUI clipboard image paste: `ctrl+v` binding, `:image` command completion, paste-temp cleanup on exit (implemented via `wl-paste` shell-out)
- TUI system message lifecycle and disambiguated `Escape` / `Ctrl+C` / cancel bindings
- TUI optimistic regen spinner and trailing user message preservation on regen
- GUI polish: streaming indicator, input auto-grow, config panel sections, hauntings and shooting stars, emotional resonance, time sync, ghost typing, settings persistence, typing combo escalation, context reverb
- `shore-matrix` re-enabled with a patched `matrix-sdk` fork
- Daemon `--instance-id` flag for stable registry IDs
- Daemon runs idle compaction inline during the autonomy tick
- CLI persists active model across sessions, adds dynamic completions, and includes character name in the log
- Autonomy: heartbeat private turns with `HEARTBEAT_OK`, `set_next_wake`, and `<sendMessage>`
- `collect_stream` client helper for request/response consumers
- Ledger/CLI: usage breakdown by call type

### Fixed
- `shore-llm-client`: strip orphan `tool_use` / `tool_result` pairs from outbound requests
- Memory agent retries on `content_filter` and transient errors
- Daemon registers the kernel-resolved port when bound to `:0`, excludes tool models from `list_models`, and applies the heartbeat model override on the warm path
- Ledger/CLI usage deserialization by column name
- `shore-mcp`: detach auto-spawned daemon via `setsid`, clamp `log_follow` seconds, exhaustive `run_cmd` match, unified `character_info` param shape, narrower spawn-on-discovery-miss, schemars alignment with rmcp

### Changed
- User-facing documentation overhaul: rewritten `README.md`, new `FEATURES.md` and `CONFIGURATION.md`, synced example config
- Revised live-testing policy in `CLAUDE.md` with structured rules distinguishing `shore-llm-client` internals from upstream code
- Drop the `enabled` Cargo feature gate on `shore-mcp` in favor of a debug-only build
- Replace `arboard` with a `wl-paste` shell-out for clipboard image paste
- Build: consolidate test binaries to reduce linker memory pressure

## [0.14.0] — 2026-04-13

### Added
- Session-scoped response routing: server routes responses back to the originating session rather than broadcasting, preventing cross-session leakage
- Truthful SWP handshake snapshots: clients receive authoritative state on connect instead of reconstructing from subsequent events
- Revisioned authoritative history sync: clients reconcile divergent local history against server revision numbers
- Request ID echoing in SWP server responses so clients can correlate requests with responses
- Direct-response guardrails test suite and CI workflow (`protocol-guardrails.yml`) to prevent protocol regressions
- Explicit remote access model: opt-in network binding with configurable `remote_access` policy (defaults to loopback-only)
- Explicit runtime invalidation boundary for character/config reloads, with covering tests in command dispatch and handler layers
- Operability pass: startup logging, README deployment notes, example config coverage for new options

### Fixed
- Daemon mutex poison recovery across autonomy, state, memory agent, and sync paths
- Registry and discovery robustness: stale-socket cleanup, connection-manager retries, and discovery path safety
- Client-side wire framing: length-prefix validation, partial-read handling, and boundary errors

### Changed
- Split monolithic `shore-daemon/src/commands/state.rs` (~2k LOC) into focused submodules (`config`, `memory`, `models`, `status`, `tests`)
- Split `shore-daemon/src/handler/mod.rs` into `command_dispatch`, `task`, and per-concern test files
- Rework daemon startup surface: cleaner main entrypoint, structured logging of bound address and mode
- Expand concurrency integration test coverage
- Reorganize `docs/todo/` plans, close out completed refactor/hardening tracks, and retire stale plan files

## [0.13.1] — 2026-04-13

### Added
- Panic policy (`docs/panic-policy.md`) and compaction responsiveness test coverage

### Fixed
- Harden compaction paths across daemon and ledger; clean remaining clippy warnings

### Changed
- Workspace-wide `cargo fmt` pass

## [0.13.0] — 2026-04-11

### Added
- Replace hidden `force-tick` with three explicit debug commands: `heartbeat_tick_now`, `heartbeat_status_dormant`, and `heartbeat_status_active`

### Fixed
- Prevent the heartbeat abandonment guard from immediately re-tripping on every tick after going dormant
- Align dormant status reporting and cache-test debug scripts with the new explicit heartbeat debug commands

## [0.12.0] — 2026-04-11

### Added
- Per-operation model selectors: `defaults.compaction` and `defaults.heartbeat` config fields allow using separate models (and API keys) for background operations, enabling budget isolation from the primary chat model

## [0.11.1] — 2026-04-11

### Fixed
- Eliminate compaction race condition by adding `CallType::Collation` to distinguish collation calls from regular compaction

## [0.11.0] — 2026-04-10

### Added
- Smart image resize pipeline: automatic resizing of oversized LLM image uploads with alpha detection, format-aware encoding, XDG disk cache, and async warm-up (default 2MB limit)
- Inline streaming thinking display, replacing the thinking popup
- `--plain` and `--content` flags for `shore log`
- `SHORE_ADDR` environment variable for daemon address override

### Fixed
- Ledger: transition `Cold→Warm` on unexpected cache read
- Downgrade cache anomaly notification urgency to normal

### Changed
- TCP-only transport: remove Unix socket support, consolidate `socket_path` + `tcp_addr` into single `addr` field, rename `--socket` to `--addr` across all clients
- Extract `shore-daemon-server` crate from `shore-daemon`
- Remove `CacheContext` plumbing from daemon handler, generation, stream, and tool loop
- Remove `cache_invalidation_warnings` config key and `Anomaly::UnexpectedRead` variant

## [0.10.1] — 2026-04-10

### Fixed
- Remove stale `forensic_character` references and fix 6 broken tests

## [0.10.0] — 2026-04-10

### Added
- `shore-test-harness` crate: `TestHarness` with daemon boot and SWP client, `MockLlmServer` with Anthropic SSE stream builder, `TestConfigBuilder`, `CollectedResponse`, and `CrashedHarness` with crash/reboot/corrupt helpers
- Comprehensive integration test suite: message roundtrip, persistence, recovery, concurrency, compaction, ledger, autonomy, tool execution, provider edge cases, and protocol validation
- Configurable cache breakpoints for debugging cache stability
- Cache forensics logging with stale request-ID fix and desktop anomaly alerts

### Fixed
- First streaming token dropped in OpenAI and Zai providers
- Protocol, config, and state-machine bugs across multiple crates
- Memory subsystem: ID collisions, timezone comparison, and error messages
- Security hardening: shell escape in notifications, symlink escape in scratchpad
- UTF-8 boundary safety for string truncation
- Cache keepalive: phantom pings, startup priming, 55min interval with 10s tick granularity, user message appended for valid API request
- Skip cache anomaly detection for heartbeat and tool-loop calls
- Move `set_next_wake` to base tool set to prevent heartbeat cache busting
- Ledger: run cache tracker for OpenRouter-routed Anthropic calls
- Daemon: create data/runtime dirs on startup

### Changed
- Migrate autonomy to `tokio::time::Instant` for deterministic test time control

## [0.9.0] — 2026-04-08

### Added
- `ConfigDuration` type for human-readable duration parsing (e.g. `"30m"`, `"2h"`)
- Renamed duration config fields to use `ConfigDuration` across all consumers
- Propagate `X-Request-ID` from client through `LlmClient` to providers for end-to-end tracing

### Fixed
- Disconnect clients after 3 consecutive broadcast lags
- Re-apply engine-locked autonomous message persistence

### Changed
- Split `handler.rs` into `handler/` module with extracted helpers
- Extract `dispatch_result_to_output` and `build_tool_result_json` tool helpers
- Remove dead config fields: `rag_results`, `rag_threshold`, `image_enabled`, `services.llm.enabled`, `services.matrix`
- Derive `image_memory_enabled` from `tool_toggles.recall_image()` instead of standalone field
- New packaging method for releases
- Add `.worktrees/` to `.gitignore`

## [0.8.0] — 2026-04-08

### Added
- Heartbeat redesign: autonomy as deadline holder with self-scheduling
- Structured logging and instrument spans across all crates (`LOGGING.md`)
- `min_wake_secs` in `HeartbeatConfig` for testability
- Split autonomy `manager.rs` into `state.rs` and `tick.rs` modules

### Fixed
- BM25 search: use `doc_freq` index for O(df) lookup instead of O(D) full scan
- Batch N+1 `get_entry` calls in semantic search with `get_entries_by_ids`
- Halve tool loop `MAX_ITERATIONS` from 40 to 20 to bound LLM call amplification
- Recover from mutex poisoning in hot path instead of panicking
- Remove dead second `take_needs_reload` check after compaction
- Replace O(n^2) string slice with `drain` to avoid per-line reallocation
- Gracefully degrade `MemoryDB::open` failure instead of killing generation
- Atomic temp+rename for `active.jsonl` writes
- Finalize `LedgerStream` on error paths to prevent unrecorded calls
- Log vector indexing errors instead of silently discarding
- Compensating-delete rollback in `compact()` pipeline
- Abort previous generation handle before spawning new one
- Route autonomous messages through engine lock to prevent race condition
- Abort in-flight generation when all clients disconnect
- Cache `MemoryDB` and `VectorStore` per character to avoid repeated opens
- Reschedule heartbeat tick on user message to prevent mid-conversation firing

### Changed
- Revert Anthropic 1h cache multiplier back to calculation time
- CI: switch package workflow to tag-based releases

## [0.7.0]

### Added
- `shore-ledger` crate: token tracking, cost accounting, cache anomaly detection
- SQLite schema for ledger with insert/query
- `CacheTracker` state machine with anomaly detection
- `PricingEngine` with OpenRouter fetch and DB cache
- `LedgerClient` wrapper with `CallType` and recording
- `LedgerStream` with finalize-or-warn pattern
- `shore usage` CLI subcommand for querying token costs and anomalies
- Expose usage command over SWP protocol for remote clients
- Route `shore memory` queries through researcher-agent pipeline
- Heartbeat tick rewrite as real multi-turn tool loop

### Fixed
- Ledger token display format and OpenRouter model ID mapping
- Startup reconstruction, `CallType`, CSV, and recalculate in ledger
- Ledger data dir resolution via `load_config` instead of bare XDG default
- Reject `shore usage` over TCP connections (local-only)
- Pricing fetch, anomaly time window, and cost display
- Anthropic cache pricing with TTL-aware multiplier
- Schema migrations for columns added after initial release
- Key model catalog by qualified name to prevent cross-provider clobbering
- Surface `send_image` results to clients
- Config: use `from_path_override` so `.env` overrides inherited env vars

### Changed
- Pre-compute cache write prices at fetch time
- Decouple SDK wire protocol from provider identity

## [0.6.0]

### Added
- TUI: fullscreen image viewer with `o` keybinding and j/k navigation
- TUI: inline image toggle with `p` keybinding
- TUI: image height clamping (80% width / 50% height of viewport)
- TUI: store pixel dimensions in `TransmittedImage`
- Silent Shore GUI: rain audio, glass cracks, DPI scaling
- GUI: streaming indicator, input auto-grow, config panel polish
- GUI: settings persistence, emotional resonance, time sync, ghost typing
- GUI: typing combo escalation, context reverb, collapsible config sections
- GUI: hauntings, shooting stars, and visual polish

### Fixed
- Heartbeat: rebuild request after compaction, improve tick prompt
- Disable `shore-matrix` to unblock build on rustc 1.94+
- Remove hardcoded `base_url` from Z.AI provider defaults
- Convert non-PNG images to PNG before kitty protocol transmission
- Memory compaction state handling improvements

## [0.5.0]

### Added
- Z.AI provider with native thinking/reasoning support
- NanoGPT provider (OpenAI-compatible route)
- Time-gap markers on user messages for temporal awareness
- Embedded Matrix homeserver with conduwuit, replacing Synapse
- Provider-specific payload projection with thinking signatures
- Interactive memory shell (`shore memory shell`)
- Per-message parameter overrides, heartbeat event log
- Reasoning effort mapping (high/medium/low/max) to Anthropic adaptive thinking
- Capture redacted thinking, DeepSeek reasoning, and Gemini thought parts
- Embed base64 image data in wire protocol for remote client support
- Activity heatmap backfill from chat history on daemon start
- Bracketed paste, image detection probe, edit messages in TUI

### Fixed
- All mechanical clippy warnings resolved
- All clippy design-level warnings resolved
- Migrate all timestamps from UTC to local-offset RFC 3339
- Suppress kitty graphics responses leaking into TUI input
- Remove synthetic "Understood." ack from inline system message conversion
- Include assistant response in `last_request` for heartbeat
- Thinking/reasoning configuration for all providers
- Autonomy init ordering

### Changed
- Extract four leaf crates from `shore-daemon`
- Consolidate duplicated provider logic into shared helpers
- Apply `cargo fmt` across entire workspace
- Move compaction/collation to `[memory]`, `cache_keepalive` to provider config
- Heartbeat journal module, unified timer interval
- Unify heartbeat and cache keepalive into single system
- Extract sub-functions from oversized `draw_conversation()` and `print_log()`

## [0.4.0]

### Added
- Multi-character support via `CharacterRegistry` with SWP handshake selection
- Client-side character state persistence
- Unified `config.toml` with nested model catalog, `include/conf.d`
- Single conversation per character (replacing multi-conversation system)
- Unified tool system with basic tools and `Send`-safe dispatch
- Rewrite memory agent as agentic LLM loop with researcher tier
- Autonomy manager with per-character tick tasks and state persistence
- CLI editor send, tool call display
- Compaction retention, recap generation, and max-messages trigger
- Stdin pipe support for send, relative message refs for edit/delete
- Collation pipeline wired into daemon, CLI, and auto-trigger
- First-class OpenRouter image generation provider
- LLM retry logic, `generate_image` implementation
- Batch implement of 10 V2-TODO items across 5 feature waves
- `get/reset_model`, `memory_changelog`, `config_check` commands, `fetch_url`
- Config schema ports and E2E test fixes
- Tavily `web_search` tool (replacing `research_web`)
- Restructured CLI from 16 to 9 commands
- Daemon-side push notifications: `notify-send`, `ntfy`, and command backends
- Defaults `display_name`, `--json` flags
- Port V1 prompt architecture: capabilities block, multi-block system, date/time, enriched compaction/collation
- Per-character config overrides via `characters/{name}/config.toml`
- TUI: vim-style command palette with tab completion and character selection
- `finish_reason` in `StreamEnd`

### Fixed
- Social need rolls use 30-min jittered intervals and cumulative probability bar
- Config command reads 'key' param to match CLI
- Non-blocking restarts, restart count reset, client retry on connect
- Message hang: stale socket, health check timeout, restart ready race
- OpenRouter missing `base_url` + Bun socket-close hang
- TUI character names, event loop double-processing, and history loading
- Handle OpenRouter-normalized reasoning field; isolate DeepSeek `reasoning_content`
- Stream timeout
- Daemon logging: switch from JSON to human-readable format
- Status command shows effective model from per-character config

### Changed
- Remove legacy `ToolRegistry` and RAG prompt injection
- Replace `message_count_threshold` with min/max/keep_recent
- Track `Cargo.lock` for reproducible binary builds
- Remove `toggle-private` and `toggle-autonomy` commands, add info subcommands
- Consolidate CLI commands and disable help subcommand
- Default `services.llm.command` to `"shore-llm"`
- Compile `shore-llm` to standalone binary via bun

## [0.3.0]

### Added
- `shore-cli`: core commands and scaffold (US-031)
- `shore-cli`: completions, images, and notifications (US-032)
- `shore-tui`: persistent connection and conversation view (US-033)
- `shore-matrix`: SWP client and Matrix SDK (US-035)
- `shore-matrix`: room management and command handling (US-036)
- `shore-matrix`: Synapse management and provisioning (US-037)
- Matrix bridge milestone (US-038)
- Data migration validation (US-039)
- Prompt template upgrade manifest (US-040)
- Config migration and V1 retirement (US-041)
- Autonomy milestone: activity tracker, heartbeat scheduler, cache keepalive (US-027–030)

## [0.2.0]

### Added
- SQLite schema and CRUD operations (US-019)
- LanceDB vector store and BM25 search (US-020)
- RAG pipeline (US-021)
- Compaction (US-022)
- Collation: 4-phase pipeline (US-023)
- Memory agent (US-024)
- Memory tools and remaining commands (US-025)
- Full memory system milestone (US-026)

## [0.1.0]

### Added
- Cargo workspace with `shore-protocol`, `shore-client`, `shore-llm`, `shore-daemon` crates
- SWP message types and shared types (US-002)
- SWP client library (US-003)
- Protocol serialization integration tests (US-004)
- `shore-llm` HTTP server scaffold and health endpoint (US-005)
- Anthropic provider (US-006)
- OpenAI-compat, OpenRouter, and ZhipuAI providers (US-007)
- Gemini provider (US-008)
- Embed and image generation endpoints (US-009)
- `shore-daemon` server: accept, route, broadcast (US-010)
- Config loading (US-011)
- Process supervision (US-012)
- Engine core: state machine, messages, conversations (US-013)
- Prompt assembly pipeline (US-014)
- LLM client and streaming consumer (US-015)
- Tool use loop and basic tools (US-016)
- Command dispatch (US-017)
- End-to-end conversation milestone (US-018)

[0.15.0]: https://github.com/eshen/silvershore/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/eshen/silvershore/compare/v0.13.2...v0.14.0
[0.13.2]: https://github.com/eshen/silvershore/compare/v0.13.1...v0.13.2
[0.13.1]: https://github.com/eshen/silvershore/compare/v0.13.0...v0.13.1
[0.13.0]: https://github.com/eshen/silvershore/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/eshen/silvershore/compare/v0.11.1...v0.12.0
[0.11.1]: https://github.com/eshen/silvershore/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/eshen/silvershore/compare/v0.10.1...v0.11.0
[0.10.1]: https://github.com/eshen/silvershore/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/eshen/silvershore/compare/v0.9.1...v0.10.0
[0.9.0]: https://github.com/eshen/silvershore/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/eshen/silvershore/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/eshen/silvershore/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/eshen/silvershore/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/eshen/silvershore/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/eshen/silvershore/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/eshen/silvershore/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/eshen/silvershore/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/eshen/silvershore/releases/tag/v0.1.0
