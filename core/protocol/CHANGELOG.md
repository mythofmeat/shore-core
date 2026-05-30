# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.6.0](https://github.com/mythofmeat/shore-core/compare/shore-protocol-v0.5.0...shore-protocol-v0.6.0) - 2026-05-30

### Fixed

- *(replay)* track provider provenance; strip non-portable thinking on provider switch ([#99](https://github.com/mythofmeat/shore-core/pull/99))

## [0.5.0](https://github.com/mythofmeat/shore-core/compare/shore-protocol-v0.4.0...shore-protocol-v0.5.0) - 2026-05-28

### Fixed

- *(usage)* render budget reset times in local AM/PM + show window in CLI ([#86](https://github.com/mythofmeat/shore-core/pull/86))

## [0.4.0](https://github.com/mythofmeat/shore-core/compare/shore-protocol-v0.3.0...shore-protocol-v0.4.0) - 2026-05-27

### Fixed

- *(compaction)* drive a tool loop; guard archive on memory writes ([#43](https://github.com/mythofmeat/shore-core/pull/43)) ([#72](https://github.com/mythofmeat/shore-core/pull/72))

## [0.3.0](https://github.com/mythofmeat/shore-core/compare/shore-protocol-v0.2.0...shore-protocol-v0.3.0) - 2026-05-22

### Breaking

- `ContentBlock::Thinking` gained a new `details: Option<serde_json::Value>`
  field carrying opaque provider-specific reasoning metadata (currently
  OpenRouter's `reasoning_details`). The variant is exhaustive, so any code
  constructing or exhaustively pattern-matching `ContentBlock::Thinking` must
  account for the new field. Existing `Thinking` payloads on the wire stay
  compatible — the field defaults to `None` and is skipped when serializing.

### Other

- [codex] stabilize OpenRouter Anthropic cache tool loops ([#29](https://github.com/mythofmeat/shore-core/pull/29))

## [0.2.0](https://github.com/mythofmeat/shore-core/compare/shore-protocol-v0.1.0...shore-protocol-v0.2.0) - 2026-05-20

### Added

- [**breaking**] remove text-to-speech support
