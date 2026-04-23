# OpenClawify Audit

Audit target: `breaking/openclawify`

Date: 2026-04-24

Scope:
- Protected self-edit activation semantics
- Memory path consistency
- Markdown-memory retrieval quality

Constraints used for the audit:
- Protected files should be staged, not live-activating
- Workspace files should remain directly editable
- `active_prompt/` should be the frozen prompt source for the current cache epoch
- Compaction/reload should be the activation boundary
- No split-brain runtime memory store

## Executive Summary

The branch is close to the target stance.

- Protected bootstrap files already use a staged activation model.
- Normal prompt assembly already reads from `active_prompt/`, not directly from the editable workspace.
- Heartbeat injection is correctly heartbeat-only for `HEARTBEAT.md`.
- The main runtime inconsistency I found was one heartbeat write path still targeting `{data_dir}/{character}/memories/` instead of the canonical workspace memory directory. That is now fixed.
- The largest remaining drift is documentation and naming: several docs still describe `memories/` or `memory/recap.md` even though the branch has moved to `workspace/memory/` plus `active_prompt/RECENT_MEMORY.md`.

I did not find evidence that protected self-edits are leaking into the active prompt before compaction/reload.

## 1. Protected Self-Edit Activation Semantics

### Current implementation

Protected bootstrap files are defined in `shore-daemon/src/memory/deferred_edits.rs`:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`

The model is:

1. Editable canonical files live in `characters/<name>/workspace/`
2. Prompt-active files live in `{data_dir}/{character}/active_prompt/`
3. Writes to protected files succeed immediately in the workspace
4. The tool layer queues a deferred activation record in `{data_dir}/{character}/deferred_edits.jsonl`
5. `apply_deferred_edits()` refreshes `active_prompt/` from the workspace and clears the queue

Key code paths:

- Protected-path normalization and queueing:
  - `shore-daemon/src/memory/deferred_edits.rs`
  - `shore-daemon/src/tools/context.rs`
  - `shore-daemon/src/tools/mod.rs`
- Snapshot seeding and refresh:
  - `ensure_active_prompt_snapshot()`
  - `refresh_active_prompt_snapshot()`
  - `apply_deferred_edits()`

### What prompt assembly actually reads

Normal chat reads bootstrap content from `active_prompt/`, not from the workspace:

- `shore-daemon/src/handler/task.rs`
  - `load_active_prompt_file(..., SOUL_FILE)`
  - `load_active_prompt_file(..., USER_FILE)`
  - `load_active_prompt_file(..., AGENTS_FILE)`
  - `load_active_prompt_file(..., TOOLS_FILE)`
  - `load_active_prompt_file(..., RECENT_MEMORY_DIGEST_FILE)`
- `shore-daemon/src/engine/prompt.rs`
  - assembles from those loaded strings

Heartbeat rebuilds do the same:

- `shore-daemon/src/autonomy/manager.rs`
  - `rebuild_request_from_disk()`
  - `load_heartbeat_instructions()`

`HEARTBEAT.md` is heartbeat-only:

- It is loaded from `active_prompt/HEARTBEAT.md`
- It is injected only inside `execute_heartbeat_tick()`
- It is not part of normal chat prompt assembly

### Assessment

This matches the desired staged-activation design:

- Workspace bootstrap files are live-editable
- The active cache prefix is frozen in `active_prompt/`
- Protected edits do not become prompt-active until compaction/reload calls `apply_deferred_edits()`

I did not find a pre-compaction path that rebuilds the chat prefix from workspace bootstrap files directly.

### Residual risks

- `ensure_active_prompt_snapshot()` is intentionally seed-only after the first snapshot exists. That is correct for cache stability, but it means broken or partially seeded snapshots can persist until compaction/reload repairs them.
- The queue file is append-only JSONL and deduped only at read time. That is fine functionally, but it can grow noisily if a character repeatedly edits the same protected file between compactions.

## 2. Memory Path Consistency

### Canonical path model on this branch

Config-side canonical memory directory:

- `characters/<name>/workspace/memory/`
- helper: `shore_config::character_memory_dir()`

Prompt-side digest:

- `{data_dir}/{character}/active_prompt/RECENT_MEMORY.md`
- helper: `recent_memory_digest_path()`

Workspace tool namespace:

- `memory/...`
- resolved by `shore-daemon/src/tools/workspace.rs`

### Where the branch is already consistent

- Tool-phase runtime wiring:
  - `shore-daemon/src/handler/generation.rs`
  - `shore-daemon/src/autonomy/manager.rs`
  - both use `character_memory_dir()`
- Command-side memory operations:
  - `shore-daemon/src/commands/state/memory.rs`
- Recent-memory digest reads/writes:
  - `shore-daemon/src/memory/compaction_impls.rs`
  - `shore-daemon/src/memory/compaction/background.rs`
- Workspace tool gating:
  - `shore-daemon/src/tools/mod.rs`
  - blocks `memory/...` when memory access is disabled

### Concrete inconsistency found

Heartbeat recap persistence was still opening:

- `{data_dir}/{character}/memories/`

inside `shore-daemon/src/autonomy/manager.rs`, while heartbeat reads and the rest of the runtime were already using:

- `characters/<name>/workspace/memory/`

That could split heartbeat daily notes away from the canonical markdown store.

### Fix applied

Changed heartbeat recap persistence to open:

- `shore_config::character_memory_dir(&lc.dirs.config, character)`

instead of `data_dir.join(character).join("memories")`.

This keeps heartbeat writes in the same single inspectable store used by:

- memory tools
- workspace `memory/...`
- command-side memory queries
- recent daily-note loading

### Remaining path drift

There is still substantial stale terminology in docs and comments:

- `docs/ARCHITECTURE.md`
- `docs/DECISIONS.md`
- `docs/QUIRKS.md`
- some older test comments

Those files still talk about `memories/` or `memory/recap.md` in places. I only corrected the most user-facing/current surfaces in this pass:

- `docs/FEATURES.md`
- `docs/CONFIGURATION.md`
- runtime/tool comments that describe protected bootstrap files or memory paths

## 3. Markdown-Memory Retrieval Quality

### Current retrieval model

There is no semantic/vector retrieval in the active runtime path.

Current retrieval is lexical and markdown-only:

- `MarkdownMemoryStore::search_text()` scores:
  - path matches
  - heading/title matches
  - content matches
  - per-token matches
- `markdown_query::answer_query()`:
  - takes top search hits
  - truncates each file
  - sends only those files to the memory-query model
- direct memory queries use `format_direct_response()`

This is coherent with the stated constraint: one inspectable source of truth, no split-brain runtime memory architecture.

### Quality assessment

Strengths:

- fully inspectable
- deterministic
- no second authoritative store
- easy to debug because ranking is local and explicit

Weaknesses:

- no synonym/stemming support
- no semantic recall for paraphrases
- ranking is still fairly shallow for broad multi-file questions
- old search snippets were front-biased, so relevant matches late in a file were easy to miss

### Fix applied

I added query-aware excerpts:

- `shore-daemon/src/memory/markdown_query.rs`
  - new `excerpt_for_query()`
- `shore-daemon/src/tools/memory_tools.rs`
  - `memory_search` now uses match-centered excerpts
- `format_direct_response()` now also uses match-centered excerpts

This is a low-risk quality improvement:

- no storage change
- no hidden index
- no cache-model change
- better retrieval ergonomics for both the character and the operator

### Recommendation

Do not add vector search back as the default runtime memory path.

Based on your stated goals, I recommend:

1. Keep markdown files as the only durable source of truth
2. Keep lexical retrieval as the default runtime mechanism
3. Improve ranking and presentation incrementally before considering semantic search

The next improvements I would prefer, in order:

1. Better lexical ranking
   - boost basename/stem matches
   - boost multi-term coverage
   - boost heading matches over body matches more aggressively
2. Better compaction output structure
   - clearer headings
   - stable topic files
   - more discoverable summaries inside the markdown itself
3. Optional ephemeral in-process indexing only
   - built from markdown at startup or on demand
   - never authoritative
   - never persisted as a separate runtime truth

I would only revisit vector search if lexical retrieval still fails after those steps and only if it is explicitly secondary to markdown, not a new shadow memory system.

## Small Safe Fixes Included In This Pass

- Fixed heartbeat daily-note persistence to use the canonical `workspace/memory/` path
- Improved memory search/direct-response excerpts to center the matching content
- Updated the most user-facing path wording from `memories/...` to `memory/...`
- Updated heartbeat integration tests to assert against the canonical workspace memory path

## Recommended Next Steps

1. Add a focused regression test for staged protected edits across compaction:
   - edit `SOUL.md`
   - verify prompt still uses old snapshot before compaction
   - compact/reload
   - verify next prompt uses the refreshed snapshot

2. Finish doc cleanup in stale architecture/decision docs so the branch stops describing:
   - `memories/`
   - `memory/recap.md`
   - old vector/SQLite runtime assumptions

3. Improve lexical ranking a little further before considering any semantic layer:
   - filename stem weighting
   - term-coverage scoring
   - heading-aware prioritization

## Bottom Line

The branch already implements the important OpenClaw-close invariant:

- editable workspace bootstrap files
- frozen active prompt snapshot
- compaction as the activation boundary

The main runtime bug was path inconsistency in heartbeat recap persistence, not a broken staged-activation model. That bug is fixed. The remaining work is mostly auditability and retrieval polish, not a core architectural rewrite.
