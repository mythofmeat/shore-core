# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [11.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v10.0.0...shore-daemon-v11.0.0) - 2026-05-31

### Breaking

- **HeartbeatEventKind now implements Copy**: The `HeartbeatEventKind` enum gained Copy semantics (copy_impl_added). Code that relied on implicit move semantics or non-Copy behavior (e.g., mutation after move, Drop implementations) will need to be updated.

  **Migration**: If your code relied on move-only semantics, explicitly use `.clone()` or adapt to copy semantics:

  ```rust
  // Before (move semantics):
  // let event = heartbeat_event_kind;
  // // event is moved, can't be used again

  // After (copy semantics):
  let event = heartbeat_event_kind; // event is copied
  // heartbeat_event_kind can still be used
  ```

- **render_template signature changed**: The `render_template` function now requires one generic type parameter where it previously required zero. All existing callsites must be updated to specify the template type explicitly.

  **Migration**: Update callsites to include the template type parameter:

  ```rust
  // Before (11.0.0):
  // render_template(template_data)?

  // After (11.0.0):
  render_template::<YourTemplateType>(template_data)?
  ```

  Replace `YourTemplateType` with the appropriate template struct for your use case.

### Other

- Correctness ratchet: strict clippy + panic hygiene + dep checks ([#114](https://github.com/mythofmeat/shore-core/pull/114))

## [10.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v9.0.0...shore-daemon-v10.0.0) - 2026-05-31

### Added

- *(tool_use)* truncate oversized tool results ([#110](https://github.com/mythofmeat/shore-core/pull/110))

## [9.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v8.0.1...shore-daemon-v9.0.0) - 2026-05-30

### Breaking

- **CompactionManager API change**: The `CompactionManager::build_final_message` method signature has changed (parameter count reduced from 4 to 3).

  **Migration**: Update all call sites to the new signature by removing the obsolete argument and adapting any dependent logic. Run your builds and tests after updating from 8.0.1 to 9.0.0 to catch any compilation errors (cargo-semver-checks will flag this breaking change).

### Other

- *(compaction)* stop inlining full memory snapshot into compact prompt ([#107](https://github.com/mythofmeat/shore-core/pull/107))

## [8.0.1](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v8.0.0...shore-daemon-v8.0.1) - 2026-05-30

### Other

- updated the following local packages: shore-llm, shore-llm, shore-ledger

## [8.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v7.0.0...shore-daemon-v8.0.0) - 2026-05-30

### Breaking

- **PromptMessage API change**: The public struct `PromptMessage` now includes a `provider_key` field to track which provider originally generated each message. This is a breaking change for any code that directly constructs `PromptMessage` instances.

  **Migration**: When constructing `PromptMessage`, you must now supply a `provider_key` field. For messages being replayed or reconstructed, use the provider key that originally generated the message. For new messages or when the provider is unknown, consider using a helper constructor or setting an appropriate default:

  ```rust
  // Old (7.x):
  // PromptMessage { role: "user", content: "...", ... }

  // New (8.0.0):
  PromptMessage {
      role: "user",
      content: "...",
      provider_key: "anthropic".to_string(), // or the appropriate provider
      ...
  }
  ```

  See [#99](https://github.com/mythofmeat/shore-core/pull/99) for context on provider provenance tracking.

### Fixed

- *(replay)* track provider provenance; strip non-portable thinking on provider switch ([#99](https://github.com/mythofmeat/shore-core/pull/99))

## [7.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v6.0.0...shore-daemon-v7.0.0) - 2026-05-29

### Added

- *(catalog)* sort discovered models alphabetically within each provider ([#91](https://github.com/mythofmeat/shore-core/pull/91))

### Fixed

- *(catalog)* apply [chat.<provider>] defaults to discovered models + kill spurious catalog warning ([#96](https://github.com/mythofmeat/shore-core/pull/96))
- *(history)* tokenized, recency-ranked search_history over chat text only ([#93](https://github.com/mythofmeat/shore-core/pull/93))

### Other

- *(config)* [**breaking**] rename max_tokens to max_output_tokens ([#94](https://github.com/mythofmeat/shore-core/pull/94))
- *(suite)* wait for observable state in heartbeat/compaction tests ([#95](https://github.com/mythofmeat/shore-core/pull/95))

## [6.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v5.0.0...shore-daemon-v6.0.0) - 2026-05-28

### Breaking

- **Compaction API changes**: The `CompactionLlm::build_initial_request` method signature now requires a `compact_now_user: serde_json::Value` parameter to support the fixed-slot system instruction pattern. Previously this was handled internally; now callers must construct and pass the compact-now user message explicitly.

  **Migration**: Update callsites to construct the `compact_now_user` message before calling `build_initial_request`:

  ```rust
  // Old (6.0.0 removed internal handling):
  // llm.build_initial_request(system, chat_request)?

  // New (6.0.0):
  let compact_now_user = json!({"role": "user", "content": compaction_prompt_text});
  llm.build_initial_request(system, compact_now_user, chat_request)?
  ```

- **Optional pre-compaction**: `run_pre_dream_compaction` now accepts `keep_turns_override: Option<usize>` to support the new `compact_to_zero` dreaming option. This allows callers to override the configured retention policy when compacting before background tasks.

### Fixed

- *(cache)* pin librarian/heartbeat system instruction at fixed slot ([#89](https://github.com/mythofmeat/shore-core/pull/89))
- *(dreaming)* gate scheduled sweeps on inactivity, max_lateness, optional pre-compaction ([#85](https://github.com/mythofmeat/shore-core/pull/85))
- *(usage)* render budget reset times in local AM/PM + show window in CLI ([#86](https://github.com/mythofmeat/shore-core/pull/86))

## [5.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v4.0.0...shore-daemon-v5.0.0) - 2026-05-28

### Breaking

- `COMPACTION_TAIL_ENTRY_COUNT` is now defined as `pub const COMPACTION_TAIL_ENTRY_COUNT: usize = 2` in `backend/daemon/src/memory/compaction_impls.rs`. `COMPACTION_TAIL_USER_PROMPT_COUNT` has been removed and is no longer exported. Downstream users importing these symbols must update their code accordingly.

### Fixed

- *(llm)* auto-route anthropic/* through Anthropic SDK + scope wrap_inline_system to slug ([#82](https://github.com/mythofmeat/shore-core/pull/82))
- *(cli)* make model setting work for discovered models + add sdk override ([#81](https://github.com/mythofmeat/shore-core/pull/81))
- *(compaction)* pin system instruction at a fixed messages slot ([#80](https://github.com/mythofmeat/shore-core/pull/80))

## [4.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v3.0.0...shore-daemon-v4.0.0) - 2026-05-27

### Fixed

- *(cli)* improve shore config output and add --toml/--all flags ([#76](https://github.com/mythofmeat/shore-core/pull/76))
- *(compaction)* drive a tool loop; guard archive on memory writes ([#43](https://github.com/mythofmeat/shore-core/pull/43)) ([#72](https://github.com/mythofmeat/shore-core/pull/72))

## [3.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v2.0.2...shore-daemon-v3.0.0) - 2026-05-22

### Breaking

- `memory::compaction::try_begin_compaction` now takes two parameters
  (`data_dir: &Path`, `character: &str`) instead of one. The single-flight lock
  is keyed on the character data root so separate daemon instances sharing a
  character name no longer collide. Callers must pass the data directory in
  addition to the character name.

### Other

- [codex] stabilize OpenRouter Anthropic cache tool loops ([#29](https://github.com/mythofmeat/shore-core/pull/29))
- *(compaction)* key single-flight lock on character data root ([#30](https://github.com/mythofmeat/shore-core/pull/30))

## [2.0.2](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v2.0.1...shore-daemon-v2.0.2) - 2026-05-22

### Fixed

- fix anthropic provider discovery ([#27](https://github.com/mythofmeat/shore-core/pull/27))

### Fixed

- Route Anthropic provider catalog refreshes through the native Anthropic
  Models API.

## [2.0.1](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v2.0.0...shore-daemon-v2.0.1) - 2026-05-21

### Other

- [codex] add role filtering to shore log ([#25](https://github.com/mythofmeat/shore-core/pull/25))

### Added

- Add optional role filtering to conversation `log`, `history_page`, and `get`
  command responses.

## [2.0.0](https://github.com/mythofmeat/shore-core/compare/shore-daemon-v1.8.5...shore-daemon-v2.0.0) - 2026-05-21

### Other

- [codex] remove Claude Code transport ([#24](https://github.com/mythofmeat/shore-core/pull/24))
- release v1.8.5 ([#21](https://github.com/mythofmeat/shore-core/pull/21))

## [1.8.5](https://github.com/mythofmeat/shore-core/releases/tag/shore-daemon-v1.8.5) - 2026-05-20

### Added

- [**breaking**] remove text-to-speech support

### Fixed

- lazy-load longer conversations

### Other

- *(release)* publish binary crates to crates.io, split arch package ([#20](https://github.com/mythofmeat/shore-core/pull/20))
- *(heartbeat)* wait for async log events ([#19](https://github.com/mythofmeat/shore-core/pull/19))
- *(heartbeat)* poll for set_next_wake event with real-time deadline
- extend wait_for_mock_requests deadline to 30s
- adopt release-plz for version bumps and changelog
- Push usage budget warnings
- Add usage budgets
- Guard Anthropic cache prefix invariants
- Serialize per-character compaction
- Align user-facing counts with turns
- Follow active chat model for background tasks when unconfigured
- Add per-key spend attribution, cost provenance, and usage-kind grouping
- Quiet and clarify service logs
- Enforce compaction-tail length in release builds (review #7)
- Branch fresh-path compaction/dreaming on SDK family
- Avoid double-read of active.jsonl during compaction (review #3)
- Align cached/fresh wire shape for compaction + dreaming (review #2)
- Warn on misconfigured background models (review #1)
- Centralize Sdk::echoes_unsigned_thinking derivation (review #6)
- Fix stale active_model assertion in e2e conversation test
- Add e2e tests pinning the 2026-05-14 refactor invariants
- Split API payload debug logs into chat / long-retention tiers
- Centralize compaction-tail shape + pin cache-breakpoint preservation
- Route ad-hoc data_dir.join(character) through character_data_dir helper
- Add canonical filename constants + character-data path helpers
- Consolidate segmented-history test fixture
- Drop redundant data_dir arg from run_compaction
- Quality follow-ups: prefix test, MessageStore, helpers
- Promote trailing-system instruction to LlmRequest::system_suffix
- Extract prepare_chat_context helper
- Centralize background-task model resolution
- Apply sampler preferences and chat tools to background memory tasks
- Merge branch 'main' into feat/message-history-range-query
- Add optional start_time/end_time range filters to search_history
- Fix cache keepalive fallback behavior
- Show archived conversation segments in history
- Fix remote desktop notifications
- Fix TTS speech request configuration
- Merge branch 'alpha' into feat/shore-notifier
- Add desktop notification listener
- Move disposable state to cache dir
- Restore history_snapshot image embedding
- Fix client disconnects when sending images
- Fix TUI image display for remote daemon connections
- Apply rustfmt to daemon sources
- Import legacy chat history for search
- Merge branch 'alpha' into fix/cache-keepalive
- Refine memory tool controls
- Replace regenerated response swipe UX with alts
- Fix TUI active model picker state
- exclude data-only directories from character listing
- use workspace-rooted paths for memory writes
- Remove built-in local embedder; require OpenAI-compatible profile
- Merge branch 'feat/claude-cli' into alpha
- Add Claude Code image attachment bridge
- auto-enable claude code subprocess
- Keep inline compaction alive after disconnect
- Harden Claude Code subprocess completion
- Fix Claude Code state rewrite regressions
- Fix Claude Code background and history regressions
- fix fixtures and background sessions
- harden keyed mcp sessions
- address provider review follow-ups
- claude_code regression probe
- claude_code provider configuration and architecture
- surface usage and Max subscription telemetry
- long-lived subprocess cache
- allocate mcp session and splice tool ledger for claude_code
- mcp-streamable-http session module
- Merge branch 'main' into worktree-claude-code-spike
- consolidate documentation
- Auto-refresh provider catalogs and bulk + completable refresh CLI
- Fold reasoning into model settings; nest matrix under connectors
- Auto-enable Anthropic prompt caching when SDK is anthropic
- Externalize hardcoded prompts into editable markdown files
- Fan out streaming generation to per-character last-user-message client
- Scope hybrid search to a path subtree instead of falling back to lexical
- Rename discovery.visibility to discovery.ignore
- Stamp current time on every heartbeat tick
- Add weekday + ISO date to in-context time markers
- Resolve bundled local embedding ids without a profile block
- Merge branch 'main' into feat/embeddings
- Tighten workspace embedding index: durable prune, linear writeback, accurate caps
- Harden workspace embedding index: atomic writes, locking, mtime freshness
- Merge branch 'feat/embeddings' of github.com:mythofmeat/silvershore into feat/embeddings
- Fix embedder cache key and enforce oversized-file skip in workspace index
- Add hybrid embeddings-backed workspace search
- Nudge model from search to read in workspace tools
- Persist heartbeat log, surface schedule in shore status
- Preserve prior-turn thinking by default
- Move dreaming state to data dir
- Surface static catalog in model_settings; default base_url for discovery
- Merge branch 'main' into feat/models-provider-overhaul
- Fix five regressions surfaced in provider-overhaul review
- Merge branch 'main' into feat/models-provider-overhaul
- Fix review regressions: search symlinks, path bypass, render cache, default validation
- Fix OpenAI-compatible reasoning replay
- Add workspace delete tool that moves files to a timestamped trash directory
- Apply rustfmt to compaction and dreaming files
- Anchor time awareness across compaction, trim, and slow conversations
- Refactor compaction prompt building for structured message handling
- Move MEMORY.md canonical path to workspace root
- Merge branch 'main' into fix/dreams
- Preserve chat cache during dreaming
- Satisfy dreaming clippy checks
- Keep dry-run dreaming diagnostic fallback
- Implement AI librarian dreaming
- Merge branch 'main' into fix/dreams
- Center workspace search excerpts on matches
- Merge branch 'dev' into fix/character-image-sending
- Reorganize workspace layout
