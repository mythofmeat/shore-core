# Invariants

These are correctness constraints for Shore. `GOALS.md` is the source of user intent; this file turns that intent into engineering constraints.

## Character Continuity

**Goal:** A character should feel continuous across days, clients, and daemon restarts.

**Must:**

- Long-running chats must survive client disconnects.
- All clients must observe the same daemon-owned state.
- Character memory must survive compaction and restart.

**Must not:**

- No client may become an alternate source of character truth.

## Markdown Memory

**Goal:** Long-term memory is inspectable, editable, and recoverable as ordinary files.

**Must:**

- Runtime memory reads and writes use `characters/{character}/workspace/memory/**/*.md`.
- Memory tools must expose paths and markdown content directly.
- Compaction must update markdown memory, not a hidden runtime database.
- Optional semantic indexes must be rebuildable ranking aids.

**Must not:**

- New runtime memory features must not depend on legacy SQLite/vector memory as authoritative state.

## Prompt Cache Preservation

**Goal:** Unexpected Anthropic cache invalidation is a high-priority bug.

**Must:**

- Normal chat prompt assembly reads protected files from `active_prompt/`.
- Workspace edits to protected prompt files remain staged until compaction/reload.
- Prompt prefix changes should have an obvious cause.
- Compaction/reload is an allowed activation boundary.

**Must not:**

- Tool use, ordinary memory-note writes, or ordinary workspace edits must not silently mutate protected active prompt files. `MEMORY.md` is intentionally prompt-visible and may change when dreaming rewrites the memory index.

## Protected Self-Edits

**Goal:** Characters can edit themselves without immediately poisoning cache stability.

**Must:**

- `SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, and `HEARTBEAT.md` edits queue deferred activation.
- Status surfaces must make pending deferred edits observable.
- Activation must refresh `active_prompt/` from workspace files.

## Compaction

**Goal:** Old conversation turns become durable continuity without losing important context.

**Must:**

- Compaction sees a bounded snapshot of existing markdown memory before writing.
- Compaction retains recent turns according to `keep_recent_turns`.
- Compaction writes markdown memory notes, not prompt recap files.
- Compaction must not write `MEMORY.md`; dreaming owns the prompt-visible index.
- Compaction activates deferred protected edits.

**Must not:**

- Compaction correctness must not require a separate collation pass.

## Heartbeat Autonomy

**Goal:** Characters can have private time without burning unbounded money or pestering absent users.

**Must:**

- Autonomy is opt-in.
- Heartbeat ticks are bounded by configured tool rounds.
- Heartbeat behavior is governed by `HEARTBEAT.md` plus runtime affordances.
- Dormancy stops autonomous LLM calls until the user returns.

**Must not:**

- Heartbeat must not force recap files or automatic daily-note writes.

## Tools

**Goal:** Tools make character interaction richer while preserving safety boundaries.

**Must:**

- Tool visibility must respect private mode and toggles.
- Memory gates must apply to durable history search and workspace `memory/...` paths.
- Workspace file tools must prevent path traversal and symlink escape.
- `exec` must not run through a shell.
- `exec` path-like arguments must remain inside the character workspace.

**Must not:**

- `exec` must not accept arbitrary executable paths or host filesystem paths.

## Scratchpad Vs Memory

**Goal:** Scratchpad and memory are different surfaces.

**Must:**

- Scratchpad is character-authored project/notes space.
- Memory is continuity/factual recall space.
- Scratchpad is only in context when the character explicitly reads it.

## Editing And Regeneration

**Goal:** The user can repair the conversation without starting over.

**Must:**

- Users can edit/delete messages by stable reference or relative index.
- Regen guidance is one-shot and does not become part of conversation canon.

## Daemon/Client Split

**Goal:** Shore behaves like a long-lived service with interchangeable clients.

**Must:**

- Daemon lifecycle is independent of clients.
- CLI, TUI, GUI, Matrix, and MCP all go through daemon/SWP boundaries.
- Closing one client must not alter character state for others.

## Remote Access

**Goal:** Remote access is explicit and honest about its limits.

**Must:**

- Non-loopback binding requires `unsafe_allow_remote_access = true`.
- `allowed_hosts` must be described only as a source-IP allowlist.

**Must not:**

- Shore must not imply built-in auth/TLS where none exists.

## Diagnostics

**Goal:** Load-bearing behavior should be inspectable.

**Must:**

- Usage, costs, cache events, errors, and tool activity should be observable enough to debug.
- Cache forensics must be available on demand.
