# Dream State Data Directory

Status: completed
Owner: agent
Started: 2026-04-30
Completed: 2026-04-30

## Goal

Move machine-readable dreaming state out of git-managed character memory config
trees and into per-character data directories.

## Context

- `backend/daemon/src/memory/dreaming.rs` stored `.dreams/*.json` under
  `characters/<Character>/workspace/memory/`.
- Human-facing memory artifacts remain in config/workspace: `DREAMS.md` and
  `MEMORY.md`.
- Data-backed character runtime state already lives under
  `$XDG_DATA_HOME/shore/<Character>/`.

## Work Items

- [x] Route dream state and staged JSON paths to the character data directory.
- [x] Keep existing legacy state readable during upgrade.
- [x] Update tests, CLI help, and behavior docs.

## Validation

- [x] `cargo test -p shore-daemon memory::dreaming`
- [x] `cargo test -p shore-daemon memory_dream_returns_useful_phase_json`
- [x] `python3 scripts/harness-check.py`

## Decisions

- 2026-04-30: Store machine JSON under `<data_dir>/<Character>/dreams/` so it
  follows other daemon-owned runtime state instead of editable memory files.
- 2026-04-30: Continue reading legacy
  `characters/<Character>/workspace/memory/.dreams/state.json` when the new
  data-backed state file is absent, so existing scheduler timestamps survive
  upgrade.

## Handoff Notes

No unresolved follow-ups.
