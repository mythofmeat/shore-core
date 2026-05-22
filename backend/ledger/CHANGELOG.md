# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.8.7](https://github.com/mythofmeat/shore-core/compare/shore-ledger-v1.8.6...shore-ledger-v1.8.7) - 2026-05-22

### Other

- updated the following local packages: shore-llm

## [1.8.6](https://github.com/mythofmeat/shore-core/compare/shore-ledger-v1.8.5...shore-ledger-v1.8.6) - 2026-05-21

### Other

- [codex] remove Claude Code transport ([#24](https://github.com/mythofmeat/shore-core/pull/24))
- release v1.8.5 ([#21](https://github.com/mythofmeat/shore-core/pull/21))

## [1.8.5](https://github.com/mythofmeat/shore-core/releases/tag/shore-ledger-v1.8.5) - 2026-05-20

### Fixed

- *(ledger)* re-fire usage warning each generation while over budget

### Other

- *(release)* publish binary crates to crates.io, split arch package ([#20](https://github.com/mythofmeat/shore-core/pull/20))
- Merge pull request #12 from mythofmeat/fix/usage-warning-refires-over-budget
- adopt release-plz for version bumps and changelog
- Fix tool-loop prompt cache tracking
- Push usage budget warnings
- Add usage budgets
- Add per-key spend attribution, cost provenance, and usage-kind grouping
- Fix OpenRouter-routed Anthropic cache-write pricing
- Centralize background-task model resolution
- Fix cache keepalive fallback behavior
- surface usage and Max subscription telemetry
- Recognize OpenRouter-routed Anthropic in usage pricing and cache health
- Fix five regressions surfaced in provider-overhaul review
- Preserve chat cache during dreaming
- Implement AI librarian dreaming
- Reorganize workspace layout
