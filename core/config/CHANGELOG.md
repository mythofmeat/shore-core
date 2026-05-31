# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.8.1](https://github.com/mythofmeat/shore-core/compare/shore-config-v0.8.0...shore-config-v0.8.1) - 2026-05-31

### Other

- Correctness ratchet: strict clippy + panic hygiene + dep checks ([#106](https://github.com/mythofmeat/shore-core/pull/106))

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
