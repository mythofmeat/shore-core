# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.7.1](https://github.com/mythofmeat/shore-core/compare/shore-protocol-v0.7.0...shore-protocol-v0.7.1) - 2026-06-04

### Other

- Enable import & literal hygiene lints ([#154](https://github.com/mythofmeat/shore-core/pull/154)) ([#185](https://github.com/mythofmeat/shore-core/pull/185))
- enable string_slice + str_to_string ([#152](https://github.com/mythofmeat/shore-core/pull/152))
- enable unsafe-block + assert-message hardening ([#156](https://github.com/mythofmeat/shore-core/pull/156))

## [0.7.0](https://github.com/mythofmeat/shore-core/compare/shore-protocol-v0.6.1...shore-protocol-v0.7.0) - 2026-06-03

### Other

- Track provider provenance per MessageAlternative ([#102](https://github.com/mythofmeat/shore-core/pull/102))
- Correctness ratchet Tier 2: draconian clippy::restriction + rustc paranoia lints ([#115](https://github.com/mythofmeat/shore-core/pull/115)) ([#144](https://github.com/mythofmeat/shore-core/pull/144))

## [0.6.1](https://github.com/mythofmeat/shore-core/compare/shore-protocol-v0.6.0...shore-protocol-v0.6.1) - 2026-05-31

### Other

- [codex] Add correctness ratchet tier 2/3 coverage ([#121](https://github.com/mythofmeat/shore-core/pull/121))
- Correctness ratchet: strict clippy + panic hygiene + dep checks ([#114](https://github.com/mythofmeat/shore-core/pull/114))

## [0.6.0](https://github.com/mythofmeat/shore-core/compare/shore-protocol-v0.5.0...shore-protocol-v0.6.0) - 2026-05-30

### Breaking

- **Message.provider_key field added**: The `Message` struct now includes a public `provider_key: Option<String>` field to track which provider originally generated each message. This is a breaking change because downstream code that exhaustively constructs or pattern-matches `Message` instances must account for the new field.

  **Migration**: Update all code that constructs `Message` to include the `provider_key` field. For messages from a known provider, populate it with the provider's key (e.g., `"anthropic"`, `"openai"`). For legacy messages or when the provider is unknown, use `None`:

  ```rust
  // Old (0.5.x):
  // Message { role: "user", content: vec![...], ... }

  // New (0.6.0):
  Message {
      role: "user",
      content: vec![...],
      provider_key: Some("anthropic".to_string()),
      ...
  }
  ```

  This change enables provider-aware message replay and allows the system to strip non-portable content (like extended thinking) when switching providers. See [#99](https://github.com/mythofmeat/shore-core/pull/99) for more context.

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
