# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.3](https://github.com/mythofmeat/shore-core/compare/shore-diagnostics-v0.2.2...shore-diagnostics-v0.2.3) - 2026-06-04

### Other

- control-flow & type-surface strictness (else_if_without_else / impl_trait_in_params) ([#155](https://github.com/mythofmeat/shore-core/pull/155)) ([#196](https://github.com/mythofmeat/shore-core/pull/196))
- integer & float arithmetic discipline ([#153](https://github.com/mythofmeat/shore-core/pull/153)) ([#194](https://github.com/mythofmeat/shore-core/pull/194))
- ban variable shadowing (shadow_same/reuse/unrelated) ([#151](https://github.com/mythofmeat/shore-core/pull/151)) ([#192](https://github.com/mythofmeat/shore-core/pull/192))
- Enable import & literal hygiene lints ([#154](https://github.com/mythofmeat/shore-core/pull/154)) ([#185](https://github.com/mythofmeat/shore-core/pull/185))
- enable string_slice + str_to_string ([#152](https://github.com/mythofmeat/shore-core/pull/152))
- enable unsafe-block + assert-message hardening ([#156](https://github.com/mythofmeat/shore-core/pull/156))

## [0.2.2](https://github.com/mythofmeat/shore-core/compare/shore-diagnostics-v0.2.1...shore-diagnostics-v0.2.2) - 2026-06-03

### Other

- enable clippy::arithmetic_side_effects on shore-daemon + diagnostics ([#148](https://github.com/mythofmeat/shore-core/pull/148)) ([#159](https://github.com/mythofmeat/shore-core/pull/159))
- Correctness ratchet Tier 2: draconian clippy::restriction + rustc paranoia lints ([#115](https://github.com/mythofmeat/shore-core/pull/115)) ([#144](https://github.com/mythofmeat/shore-core/pull/144))

## [0.2.1](https://github.com/mythofmeat/shore-core/compare/shore-diagnostics-v0.2.0...shore-diagnostics-v0.2.1) - 2026-05-31

### Other

- Correctness ratchet: strict clippy + panic hygiene + dep checks ([#114](https://github.com/mythofmeat/shore-core/pull/114))

## [0.2.0](https://github.com/mythofmeat/shore-core/compare/shore-diagnostics-v0.1.1...shore-diagnostics-v0.2.0) - 2026-05-21

### Other

- [codex] remove Claude Code transport ([#24](https://github.com/mythofmeat/shore-core/pull/24))

## [0.1.1](https://github.com/mythofmeat/shore-core/compare/shore-diagnostics-v0.1.0...shore-diagnostics-v0.1.1) - 2026-05-19

### Other

- fix flaky test by stripping ANSI escapes
