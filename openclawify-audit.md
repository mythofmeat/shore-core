# OpenClawify Audit

Date: 2026-04-24

Status: complete for current `HEAD` (`OpenClawify heartbeat and dreaming semantics`).

## Bottom Line

The branch implements the intended OpenClaw-style stance and the later heartbeat/dreaming correction:

- character workspaces are editable markdown files
- long-term memory is `workspace/memory/**/*.md`
- protected prompt files are staged through `active_prompt/`
- compaction/reload is the activation boundary
- heartbeat autonomy is a private scheduled turn governed by the active `HEARTBEAT.md`
- heartbeat does not force recaps, daily-note writes, or hardcoded maintenance behavior
- `HEARTBEAT_OK` is suppressed, `<sendMessage>...</sendMessage>` is delivered, and `set_next_wake` schedules the next wake
- dreaming is the opt-in scheduled memory consolidation path, independent of heartbeat
- dreaming stages machine state in `workspace/memory/.dreams/`, writes review output to `workspace/memory/DREAMS.md`, and promotes durable facts to `workspace/memory/MEMORY.md`
- optional hybrid retrieval is a rebuildable ranking aid, not source of truth

## Ship-Risk Items Addressed

- Heartbeat no longer persists a recap or writes memory unless the model explicitly uses a write-capable tool.
- Workspace memory access is gated consistently with memory tool toggles.
- Protected self-edits remain deferred until explicit activation.
- `exec` now rejects executable paths and path-like arguments outside the character workspace.
- User-facing docs have been rewritten around `GOALS.md` and current branch behavior.
- Dreaming config defaults are opt-in and validated, with CLI support for `shore memory dream --status`, `--dry-run`, and `--force`.
- Dreaming internals are excluded from normal memory listing/search/promotion context unless read explicitly by path.
- The final branch verification passed locally:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - `cargo build --workspace --release`
  - CLI smoke checks for `shore --help`, `shore-daemon --help`, `shore memory --help`, and `shore memory dream --help`

## Remaining External Check

One live provider smoke test is still outside the local verification envelope because it requires real credentials and may spend money:

1. start daemon with a test character
2. send a normal message
3. ask a memory question
4. run `shore memory compact`
5. confirm memory markdown and `active_prompt/RECENT_MEMORY.md` update
6. confirm no unexpected cache behavior in the ledger

That is not a known code/documentation gap in this branch; it is an operational pre-release smoke check.
