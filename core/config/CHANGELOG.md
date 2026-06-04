# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.1](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.10.0...shore-config-v0.10.1) - 2026-06-04

### Other

- enable unsafe-block + assert-message hardening ([#156](https://github.com/mythofmeat/shore-core/pull/156))

## [0.10.0](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.9.0...shore-config-v0.10.0) - 2026-06-03

### Other

- Address CodeRabbit review (PR #176)
- Deprecate static [chat.*]/[tools.*] catalog ([#139](https://github.com/mythofmeat/shore-core/pull/139))
- Address CodeRabbit review on #172 ([#164](https://github.com/mythofmeat/shore-core/pull/164))
- Remove the dead `thinking_enabled` model setting ([#164](https://github.com/mythofmeat/shore-core/pull/164))
- Make `reasoning_effort = "off"` settable on Kimi/moonshot ([#164](https://github.com/mythofmeat/shore-core/pull/164))
- Native DeepSeek + Moonshot providers via the Vercel AI SDK ([#164](https://github.com/mythofmeat/shore-core/pull/164))
- Per-model OpenRouter capability resolution by model id ([#164](https://github.com/mythofmeat/shore-core/pull/164))
- Make Gemini 3.1 effort override Pro-specific (CodeRabbit #171)
- Ground capability matrix in provider docs; fix Gemini 3.x Pro effort domain ([#166](https://github.com/mythofmeat/shore-core/pull/166))
- Unify embedding/image_generation onto provider:model_id shape ([#140](https://github.com/mythofmeat/shore-core/pull/140)) ([#169](https://github.com/mythofmeat/shore-core/pull/169))
- Capability-aware `shore model setting` + single-source capabilities.toml ([#162](https://github.com/mythofmeat/shore-core/pull/162)) ([#165](https://github.com/mythofmeat/shore-core/pull/165))
- Per-sdk capability matrix in code + provider/sdk tiebreak ([#138](https://github.com/mythofmeat/shore-core/pull/138)) ([#161](https://github.com/mythofmeat/shore-core/pull/161))
- Recover #137: rehome per-provider defaults onto [providers.*.defaults] (stranded by merge race) ([#160](https://github.com/mythofmeat/shore-core/pull/160))
- Correctness ratchet Tier 2: draconian clippy::restriction + rustc paranoia lints ([#115](https://github.com/mythofmeat/shore-core/pull/115)) ([#144](https://github.com/mythofmeat/shore-core/pull/144))

## [0.9.0](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.8.1...shore-config-v0.9.0) - 2026-06-02

### Breaking

- shore-config (0.9.0) — breaking changes to API types and enum discriminants due to LLM sidecar migration ([#123](https://github.com/mythofmeat/shore-core/pull/123)) and OpenRouter SDK consolidation ([#128](https://github.com/mythofmeat/shore-core/pull/128)); consumers must update usages accordingly

## [0.8.1](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.8.0...shore-config-v0.8.1) - 2026-05-31

### Other

- [codex] Add correctness ratchet tier 2/3 coverage ([#121](https://github.com/mythofmeat/shore-core/pull/121))
- Correctness ratchet: strict clippy + panic hygiene + dep checks ([#114](https://github.com/mythofmeat/shore-core/pull/114))

## [0.8.0](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.7.0...shore-config-v0.8.0) - 2026-05-31

### Added

- *(tool_use)* truncate oversized tool results ([#110](https://github.com/mythofmeat/shore-core/pull/110))

## [0.7.0](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.6.0...shore-config-v0.7.0) - 2026-05-29

### Fixed

- *(catalog)* apply [chat.<provider>] defaults to discovered models + kill spurious catalog warning ([#96](https://github.com/mythofmeat/shore-core/pull/96))

### Other

- *(config)* [**breaking**] rename max_tokens to max_output_tokens ([#94](https://github.com/mythofmeat/shore-core/pull/94))

## [0.6.0](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.5.0...shore-config-v0.6.0) - 2026-05-28

### Fixed

- *(cache)* pin librarian/heartbeat system instruction at fixed slot ([#89](https://github.com/mythofmeat/shore-core/pull/89))
- *(dreaming)* gate scheduled sweeps on inactivity, max_lateness, optional pre-compaction ([#85](https://github.com/mythofmeat/shore-core/pull/85))

## [0.5.0](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.4.0...shore-config-v0.5.0) - 2026-05-27

### Fixed

- *(cache)* mirror TS daemon-ts cache behavior + wire-shape tests ([#71](https://github.com/mythofmeat/shore-core/pull/71))
- *(compaction)* drive a tool loop; guard archive on memory writes ([#43](https://github.com/mythofmeat/shore-core/pull/43)) ([#72](https://github.com/mythofmeat/shore-core/pull/72))

## [0.4.0](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.3.0...shore-config-v0.4.0) - 2026-05-21

### Other

- [codex] remove Claude Code transport ([#24](https://github.com/mythofmeat/shore-core/pull/24))

## [0.3.0](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.2.0...shore-config-v0.3.0) - 2026-05-20

### Added

- *(usage)* add custom reset anchors for usage budgets

## [0.2.0](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.1.0...shore-config-v0.2.0) - 2026-05-20

### Added

- [**breaking**] remove text-to-speech support
