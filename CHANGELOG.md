# Changelog

All notable changes to Shore are documented in this file.
Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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
- Skip cache anomaly detection for interiority and tool-loop calls
- Move `set_next_wake` to base tool set to prevent interiority cache busting
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
- Interiority redesign: autonomy as deadline holder with self-scheduling
- Structured logging and instrument spans across all crates (`LOGGING.md`)
- `min_wake_secs` in `InteriorityConfig` for testability
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
- Reschedule interiority tick on user message to prevent mid-conversation firing

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
- Interiority tick rewrite as real multi-turn tool loop

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
- Interiority: rebuild request after compaction, improve tick prompt
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
- Include assistant response in `last_request` for interiority
- Thinking/reasoning configuration for all providers
- Autonomy init ordering

### Changed
- Extract four leaf crates from `shore-daemon`
- Consolidate duplicated provider logic into shared helpers
- Apply `cargo fmt` across entire workspace
- Move compaction/collation to `[memory]`, `cache_keepalive` to provider config
- Interiority journal module, unified timer interval
- Unify interiority and cache keepalive into single system
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
