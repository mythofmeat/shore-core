# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [5.0.0](https://github.com/mythofmeat/shore-core/compare/shore-llm-v4.0.1...shore-llm-v5.0.0) - 2026-05-28

### Breaking

- **`LlmRequest::system_suffix` field removed**: The `system_suffix: Option<String>` field has been removed from `LlmRequest` and replaced with the `push_inline_system(&mut self, content: impl Into<String>)` method. The old field was a footgun that caused cache-prefix drift during tool loops because `preprocess_request` re-expanded it at the current tail on every `generate()` call.

  **Migration**: Replace all uses of `system_suffix` with `push_inline_system`:

  ```rust
  // Old (5.0.0 removed this):
  // let mut request = LlmRequest {
  //     system_suffix: Some("Be concise.".into()),
  //     ..
  // };

  // New (5.0.0):
  let mut request = LlmRequest { /* fields */ };
  request.push_inline_system("Be concise.");
  ```

  The new method appends a `role:"system"` message at a fixed index in the `messages` array, preserving Anthropic's content-addressed prefix cache across tool-loop iterations. See PRs [#80](https://github.com/mythofmeat/shore-core/pull/80), [#84](https://github.com/mythofmeat/shore-core/pull/84), and [#89](https://github.com/mythofmeat/shore-core/pull/89) for implementation details.

### Fixed

- *(cache)* pin librarian/heartbeat system instruction at fixed slot ([#89](https://github.com/mythofmeat/shore-core/pull/89))

## [4.0.1](https://github.com/mythofmeat/shore-core/compare/shore-llm-v4.0.0...shore-llm-v4.0.1) - 2026-05-28

### Fixed

- *(llm)* auto-route anthropic/* through Anthropic SDK + scope wrap_inline_system to slug ([#82](https://github.com/mythofmeat/shore-core/pull/82))
- *(compaction)* pin system instruction at a fixed messages slot ([#80](https://github.com/mythofmeat/shore-core/pull/80))

## [4.0.0](https://github.com/mythofmeat/shore-core/compare/shore-llm-v3.0.0...shore-llm-v4.0.0) - 2026-05-27

### Fixed

- *(cache)* mirror TS daemon-ts cache behavior + wire-shape tests ([#71](https://github.com/mythofmeat/shore-core/pull/71))
- *(compaction)* drive a tool loop; guard archive on memory writes ([#43](https://github.com/mythofmeat/shore-core/pull/43)) ([#72](https://github.com/mythofmeat/shore-core/pull/72))

## [3.0.0](https://github.com/mythofmeat/shore-core/compare/shore-llm-v2.0.1...shore-llm-v3.0.0) - 2026-05-22

### Breaking

- `StreamEvent` added a new `ReasoningDetails { details: serde_json::Value }`
  variant for opaque provider-side reasoning metadata. The enum is exhaustive,
  so downstream `match` arms must handle the new variant.
- Inserting `ReasoningDetails` shifted the discriminants of the variants that
  follow it: `RedactedThinking` (4 → 5), `ToolUse` (5 → 6), and `Done`
  (6 → 7). Any consumer relying on the numeric discriminant via `as isize`
  (e.g. in FFI bindings or wire encodings) must be regenerated.

### Other

- [codex] stabilize OpenRouter Anthropic cache tool loops ([#29](https://github.com/mythofmeat/shore-core/pull/29))

## [2.0.1](https://github.com/mythofmeat/shore-core/compare/shore-llm-v2.0.0...shore-llm-v2.0.1) - 2026-05-22

### Fixed

- fix anthropic provider discovery ([#27](https://github.com/mythofmeat/shore-core/pull/27))

### Fixed

- Use Anthropic's native Models API headers and metadata shape for provider
  discovery.

## [2.0.0](https://github.com/mythofmeat/shore-core/compare/shore-llm-v1.8.5...shore-llm-v2.0.0) - 2026-05-21

### Other

- [codex] remove Claude Code transport ([#24](https://github.com/mythofmeat/shore-core/pull/24))
- release v1.8.5 ([#21](https://github.com/mythofmeat/shore-core/pull/21))

## [1.8.5](https://github.com/mythofmeat/shore-core/releases/tag/shore-llm-v1.8.5) - 2026-05-20

### Fixed

- fixed another claude caching during tool use issue

### Other

- *(release)* publish binary crates to crates.io, split arch package ([#20](https://github.com/mythofmeat/shore-core/pull/20))
- adopt release-plz for version bumps and changelog
- Switch property-matrix modulo checks to is_multiple_of
- Move LLM request timeout off the shared client onto per-call generates
- Guard Anthropic cache prefix invariants
- Add per-key spend attribution, cost provenance, and usage-kind grouping
- Add e2e tests pinning the 2026-05-14 refactor invariants
- Split API payload debug logs into chat / long-retention tiers
- Centralize compaction-tail shape + pin cache-breakpoint preservation
- Single source of truth for <system_instruction> tag spelling
- Collapse zai translate_messages into openai via ProviderContext flags
- Promote trailing-system instruction to LlmRequest::system_suffix
- Move disposable state to cache dir
- Remove built-in local embedder; require OpenAI-compatible profile
- Add Claude Code image attachment bridge
- Reject Claude Code image input early
- Add Claude Code native session replay
- Document Claude Code image input gap
- Add Claude Code partial streaming
- Extend Claude Code subprocess retention
- Harden Claude Code subprocess completion
- Fix Claude Code state rewrite regressions
- Fix Claude Code background and history regressions
- fix fixtures and background sessions
- harden keyed mcp sessions
- address provider review follow-ups
- claude_code regression probe
- surface usage and Max subscription telemetry
- long-lived subprocess cache
- allocate mcp session and splice tool ledger for claude_code
- Merge branch 'main' into worktree-claude-code-spike
- consolidate documentation
- Auto-refresh provider catalogs and bulk + completable refresh CLI
- Merge branch 'feat/embeddings' of github.com:mythofmeat/silvershore into feat/embeddings
- Preserve prior-turn thinking by default
- Surface static catalog in model_settings; default base_url for discovery
- Merge branch 'main' into feat/models-provider-overhaul
- Fix five regressions surfaced in provider-overhaul review
- Fix five regressions in provider-overhaul model resolution
- Merge remote-tracking branch 'origin/main' into feat/models-provider-overhaul
- Fix OpenAI-compatible reasoning replay
- Move MEMORY.md canonical path to workspace root
- Reorganize workspace layout
