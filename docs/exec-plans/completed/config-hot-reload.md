# Config Hot Reload

Status: completed
Owner: agent
Started: 2026-04-29

## Goal

Reload daemon runtime configuration automatically when supported config files change, without restarting the daemon or activating protected prompt workspace edits.

## Context

- TODO: `TODO.md`
- Existing manual reload: `shore config --reset` / `config_reset`
- Config docs: `CONFIGURATION.md`
- Prompt activation invariants: `docs/dev-info/INVARIANTS.md`

## Work Items

- [x] Factor shared runtime reload application for manual and automatic reloads.
- [x] Add filtered `notify` watcher with debounce.
- [x] Route watcher events into the daemon handler.
- [x] Cover reload filtering and runtime semantics with focused tests.
- [x] Update docs and mark TODO complete.

## Validation

- [x] `cargo fmt --all --check`
- [x] focused daemon/config tests
- [x] `cargo test -p shore-daemon --lib`
- [x] `python3 scripts/harness-check.py`

## Decisions

- 2026-04-29: Hot reload is always active and runtime-only.
- 2026-04-29: Watch config inputs but exclude prompt workspace/memory files to preserve prompt activation boundaries.

## Handoff Notes

The manual reset path should continue clearing runtime overrides; automatic reload should preserve them.
