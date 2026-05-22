# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.8.8](https://github.com/mythofmeat/shore-core/compare/shore-cli-v1.8.7...shore-cli-v1.8.8) - 2026-05-22

### Fixed

- *(cli)* align shore usage columns and surface local reset time ([#33](https://github.com/mythofmeat/shore-core/pull/33))

### Other

- [codex] stabilize OpenRouter Anthropic cache tool loops ([#29](https://github.com/mythofmeat/shore-core/pull/29))

### Fixed

- Stop waiting for another stream turn after a terminal tool-use response.

## [1.8.7](https://github.com/mythofmeat/shore-core/compare/shore-cli-v1.8.6...shore-cli-v1.8.7) - 2026-05-21

### Other

- [codex] add role filtering to shore log ([#25](https://github.com/mythofmeat/shore-core/pull/25))

### Added

- Add `shore log --role` filtering for user, assistant, and system messages,
  with `character` accepted as an assistant-role alias.

## [1.8.6](https://github.com/mythofmeat/shore-core/compare/shore-cli-v1.8.5...shore-cli-v1.8.6) - 2026-05-21

### Other

- [codex] remove Claude Code transport ([#24](https://github.com/mythofmeat/shore-core/pull/24))
- release v1.8.5 ([#21](https://github.com/mythofmeat/shore-core/pull/21))

## [1.8.5](https://github.com/mythofmeat/shore-core/releases/tag/shore-cli-v1.8.5) - 2026-05-20

### Added

- [**breaking**] remove text-to-speech support

### Fixed

- lazy-load longer conversations
- fix heartbeat dreaming and memory overlaps

### Other

- *(release)* publish binary crates to crates.io, split arch package ([#20](https://github.com/mythofmeat/shore-core/pull/20))
- adopt release-plz for version bumps and changelog
- Push usage budget warnings
- Add usage budgets
- Prettify tool block formatting
- Align user-facing counts with turns
- Add per-key spend attribution, cost provenance, and usage-kind grouping
- Add hour-based usage windows (e.g. --last 4h)
- Show archived conversation segments in history
- Fix remote desktop notifications
- Add desktop notification listener
- Fix client disconnects when sending images
- Replace regenerated response swipe UX with alts
- surface usage and Max subscription telemetry
- Auto-refresh provider catalogs and bulk + completable refresh CLI
- Fold reasoning into model settings; nest matrix under connectors
- Rename discovery.visibility to discovery.ignore
- Persist heartbeat log, surface schedule in shore status
- Move dreaming state to data dir
- Merge branch 'main' into feat/models-provider-overhaul
- Fix five regressions surfaced in provider-overhaul review
- Fix five regressions in provider-overhaul model resolution
- Surface provider + sampler commands in the CLI and TUI
- Add per-provider API key fallback with non-sticky rotation
- Wire preferences into dispatch, commands, and generation path
- Merge branch 'main' into fix/dreams
- Reorganize workspace layout
