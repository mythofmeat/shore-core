Completion status

- [x] Merged from main.
- [x] Implemented the AI/tool-loop dreaming librarian path.
- [x] Updated scheduler, command path, model resolution, audit fallback, generated-artifact exclusions, protected-file handling, dry-run behavior, docs, and focused tests.
- [x] Replaced the old deterministic sweep as the production dreaming path; the old scoring sweep remains only as a legacy diagnostic fallback.

Goal

Implement the actual Shore dreaming feature we discussed: dreaming should become a character-led memory librarian pass, not a deterministic keyword/scoring sweep.

Dreaming’s job is to make the character’s markdown memory easier to search and recall by:

- reading/listing/searching existing memory files
- sorting long-term facts out of daily/raw notes
- deduplicating repeated information
- moving or consolidating information into better files
- noticing stale/superseded/incorrect information
- updating `workspace/memory/MEMORY.md` as the prompt-visible memory index
- writing `workspace/memory/DREAMS.md` as the audit diary of what happened

This task should implement the feature, not merely clean up docs or rename variables.

Work on `main`.

Context

The branch currently has the start of the new architecture:

- Prompt assembly uses `memory_index` and injects `<memory_index>` from `workspace/memory/MEMORY.md`.
- Compaction no longer owns the prompt recap and should not write `MEMORY.md`.
- `MEMORY.md` is intended to be the prompt-visible memory index.
- Dreaming currently appears to be a deterministic Light/REM/Deep candidate scoring/indexing pass. That is not enough.

The desired architecture is:

```text
conversation
  ↓
compaction captures/preserves older material into markdown memory files
  ↓
dreaming periodically acts as a character-led memory librarian
  ↓
dreaming reorganizes/dedupes/sorts memory files and updates MEMORY.md
  ↓
normal prompts inject MEMORY.md as <memory_index>
````

Important conceptual split:

* `SOUL.md` = character identity
* `USER.md` = durable user profile
* `AGENTS.md` = standing behavior/collaboration instructions
* `TOOLS.md` = tool-use guidance
* `HEARTBEAT.md` = heartbeat-only guidance
* `memory/MEMORY.md` = index/map of the memory folder, recently updated files, and still-relevant conversational throughlines
* other files under `memory/` = actual long-term memory notes, daily logs, project/topic notes, etc.
* `memory/DREAMS.md` = human-readable audit diary of dreaming passes
* `memory/.dreams/` = machine-readable staging/debug state

Do not make `MEMORY.md` a dump of all facts. It should be an orientation map.

Suggested files/modules to inspect

Prompt/index path:

* `backend/daemon/src/engine/prompt.rs`
* `backend/daemon/src/handler/task.rs`
* `backend/daemon/src/memory/deferred_edits.rs`

Compaction:

* `backend/daemon/src/memory/compaction/parser.rs`
* `backend/daemon/src/memory/compaction/mod.rs`
* `backend/daemon/src/memory/compaction/types.rs`
* `backend/daemon/src/memory/compaction_impls.rs`

Dreaming:

* `backend/daemon/src/memory/dreaming.rs`
* `backend/daemon/src/autonomy/manager.rs`
* `core/config/src/app.rs` or wherever `DreamingConfig` is defined

Tool loop / workspace tools:

* `backend/daemon/src/engine/tools.rs`
* `backend/daemon/src/tools/mod.rs`
* `backend/daemon/src/tools/context.rs`
* workspace `read`, `write`, `edit`, `list_files`, `search`
* `search_history`, if available and appropriate
* existing `SharedToolContext` / `ToolContext` setup in normal generation and heartbeat paths

Docs/tests:

* `FEATURES.md`
* `CONFIGURATION.md`
* `ARCHITECTURE.md`
* `CHANGELOG.md`
* existing prompt, compaction, dreaming, and tool-loop tests

Implementation requirements

1. Replace deterministic dreaming as the primary behavior

The current deterministic Light/REM/Deep scoring/indexing sweep should not remain the main implementation.

Acceptable options:

* Remove it.
* Keep it only as a fallback when no LLM/tool-loop dependencies are available.
* Keep pieces only for dry-run diagnostics, but the real non-dry-run dreaming path must be LLM/tool-loop based.

The main dreaming behavior must involve a model call where the character is instructed to use memory tools to inspect and reorganize memory files.

2. Make dreaming an LLM/tool-loop librarian pass

Implement a dreaming runner that can call the LLM with a dedicated dreaming/librarian prompt and a tool context that exposes memory workspace tools.

The dreaming pass should be able to:

* list memory files
* read memory files
* search memory files
* write/edit memory files
* update `MEMORY.md`
* append/update `DREAMS.md`
* optionally search conversation history if the existing tool system supports it safely

The model should not be forced to use a rigid folder taxonomy. It should inspect the existing memory layout and improve it.

The memory folder is self-organizing. `MEMORY.md` is the table of contents.

3. Add an explicit dreaming prompt

Add a dedicated prompt template for dreaming, either inline or loaded from a configurable/template file if the project already has a template convention.

The prompt should say, in substance:

```text
You are running a private memory maintenance pass.

Your task is not to chat with the user. Your task is to make your markdown memory easier for future-you to search and recall.

Use your memory tools to inspect existing memory files. Organize information so durable long-term facts are easy to find. Prefer updating existing files over creating duplicates. Move durable facts out of daily/raw logs into appropriate long-term files when useful. Deduplicate repeated facts. Mark stale/superseded/incorrect information clearly rather than preserving contradictions as equally current. Leave uncertain cases in a review/needs-review area.

You must maintain MEMORY.md as the prompt-visible memory index. MEMORY.md should include:
- an overview of important memory files and what they contain
- recently updated or worth-reading files
- ongoing conversational throughlines that remain relevant
- any unresolved memory-maintenance questions or contradictions

MEMORY.md is not the full memory itself. It should not duplicate SOUL.md, USER.md, AGENTS.md, TOOLS.md, or HEARTBEAT.md.

Write an audit entry to DREAMS.md describing:
- what files you inspected
- what files you changed
- what you moved/deduped/superseded
- what unresolved issues remain

Do not send a user-facing message.
```

Use the actual character name/display name/config where appropriate.

4. Wire the dreaming pass into the existing scheduler

`run_sweep(...)` currently has a narrow signature. Update the dreaming execution path so non-dry-run dreaming has access to the dependencies it needs:

* loaded config
* LLM client / ledger client
* model resolution
* workspace/memory directory
* tool context
* configured dreaming max tool rounds

This may require changing the signature of `run_sweep` or adding a new function such as:

```rust
run_librarian_sweep(...)
```

or

```rust
run_ai_dreaming_sweep(...)
```

Then update the autonomy tick path to call the real implementation when dreaming is due.

Do not fake this by producing a deterministic `MEMORY.md` from file listings only.

5. Choose the correct model

Use an appropriate configured model for dreaming.

Preferred behavior:

* If there is a dedicated dreaming model config, use it.
* Otherwise use the normal default chat model or compaction model, whichever fits the existing config model architecture best.
* Track the ledger call type clearly. If there is no `CallType::Dreaming`, add one if appropriate; otherwise use the closest existing background/memory call type and document it.

Do not silently use the heartbeat model unless that is explicitly the intended background model for memory work.

6. Tool loop behavior

Dreaming should use a private tool loop, not a normal user-visible generation.

Requirements:

* No user message should be sent.
* No SWP user-visible chat response should be emitted.
* Tool calls may be logged diagnostically if that is normal.
* The loop should stop after `memory.dreaming.max_tool_rounds`.
* The final model response should be treated as an internal report/summary, not a chat message.
* The final report should be recorded in `DREAMS.md` if the model did not already do so.

If reusing the normal tool-loop implementation requires a synthetic request/result, do so cleanly and add tests. If that is too awkward, create a narrower private tool-loop helper for background tasks.

7. Maintain `MEMORY.md` contract

After a successful dreaming pass, `workspace/memory/MEMORY.md` should exist and be useful.

It should generally look like:

```md
# Memory Index

This file is the character's map of long-term memory. It is not the full memory itself.
Use it to decide which memory files to inspect before answering.

Core user facts and standing behavior guidance are already loaded from USER.md and AGENTS.md; do not duplicate them here unless needed as pointers to memory files.

## Memory areas

- `some/file.md` — what this file contains and when to read it.

## Recently updated files

- `some/file.md` — why it was recently updated.

## Current conversational throughlines

- Still-relevant ongoing context that helps future recall.

## Needs review

- Unresolved contradictions, stale memories, or uncertain filing decisions.
```

Do not hardcode exact folder names like `people/`, `projects/`, or `preferences/` as required. The model may create/use them if useful, but the layout should remain flexible.

8. Maintain `DREAMS.md` as audit diary

After a successful dreaming pass, `workspace/memory/DREAMS.md` should contain an audit entry.

The entry should be human-readable and should include:

* timestamp
* that this was an AI librarian dreaming pass
* files inspected
* files changed
* important moves/dedupes/supersessions
* unresolved issues
* whether `MEMORY.md` was updated

If the model fails to write `DREAMS.md`, the Rust side should append a minimal fallback audit entry so there is always a trace.

9. Preserve generated artifact exclusions

Generated dreaming files should not be treated as ordinary memory sources for future librarian passes.

Exclude from candidate/source ingestion and/or prompt source lists:

* `.dreams/**`
* `DREAMS.md`
* `dreams.md`
* `MEMORY.md`
* `dreaming/**`

But the librarian pass may read `MEMORY.md` to update the index and may read `DREAMS.md` if needed for audit continuity. The key is that these should not be mined as durable memory facts.

10. Keep compaction separate

Do not make compaction write `MEMORY.md`.

Do not reintroduce `<recap>` as the prompt-visible memory mechanism.

Compaction should continue to:

* archive old turns
* retain recent turns
* write normal markdown memory notes
* avoid generated/index paths
* activate protected prompt edits at the cache boundary

11. Protected prompt files

Dreaming should not silently rewrite `SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, or `HEARTBEAT.md`.

If the librarian notices information that belongs in a protected prompt file, it should record that in `MEMORY.md` under Needs Review and/or in `DREAMS.md`.

Only use the existing deferred protected edit path if the project already supports it safely and tests cover it. Otherwise leave protected-file edits out of this implementation.

12. Dry-run behavior

Dry-run dreaming should not write files.

It should still execute enough planning to be useful.

Acceptable dry-run result:

* model/tool-loop can inspect files
* no writes/edits are applied, or write tools are replaced with no-op/captured versions
* result reports what would change

If no safe no-op write-tool mode exists, implement dry-run by not enabling write/edit tools and ask the model to produce a proposed plan. Make this explicit in the output.

13. Remove or downgrade misleading fields

The current dreaming structs may use terms like `promoted`, `promotion_score`, `indexed`, etc.

Do not let old naming drive the behavior.

Preferred terms:

* inspected
* changed
* indexed
* throughlines
* unresolved
* audit
* librarian pass

If changing public structs is too disruptive, keep compatibility fields but document them as legacy/compatibility aliases and make the behavior correct.

14. Fix docs and merge-conflict damage

Also fix any obvious doc breakage encountered during this implementation.

In particular, remove unresolved merge conflict markers from `FEATURES.md`.

Docs should clearly say:

* `MEMORY.md` is prompt-visible and replaces the old recap/digest concept.
* Compaction captures/preserves.
* Dreaming organizes/collates/sorts as a character-led librarian pass.
* Memory folder layout is flexible.
* `USER.md` and `AGENTS.md` remain pinned prompt files and should not be duplicated by `MEMORY.md`.
* `DREAMS.md` is audit/review output.
* `.dreams/` is machine-readable staging/debug output.

Constraints

* Implement the feature. Do not stop at docs/test cleanup.
* Work on `main`.
* Do not restore the old main-branch SQLite/vector/RAG/collation architecture.
* Do not add a hidden authoritative memory database or claim ledger.
* Markdown memory files remain the source of truth.
* Do not impose a rigid folder taxonomy under `memory/`.
* Do not make `MEMORY.md` duplicate `USER.md` or `AGENTS.md`.
* Do not make compaction write `MEMORY.md`.
* Do not send user-visible messages from dreaming.
* Keep the tool loop bounded by config.
* Prefer small, reviewable file edits.
* Keep behavior deterministic where Rust is responsible, but the actual memory organization decision should be model/tool-loop driven.

Validation steps

Run:

```sh
grep -R "<<<<<<<\|=======\|>>>>>>>" -n .
grep -R "RECENT_MEMORY\|recent_memory_digest\|recent_memory" -n backend core clients bridges docs FEATURES.md CONFIGURATION.md ARCHITECTURE.md CHANGELOG.md examples || true
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Add/update focused tests for:

1. Prompt assembly injects `MEMORY.md` as `<memory_index>`.
2. Private conversations suppress memory index injection.
3. Compaction does not require or write recap.
4. Compaction refuses `MEMORY.md`, `DREAMS.md`, `.dreams/**`, and `dreaming/**` writes.
5. Dreaming non-dry-run invokes an LLM/tool-loop librarian path, not the deterministic scoring-only path.
6. Dreaming can list/read/search memory files.
7. Dreaming can update normal memory files and `MEMORY.md`.
8. Dreaming writes or fallback-appends an audit entry to `DREAMS.md`.
9. Dry-run dreaming does not write files.
10. Generated dreaming artifacts are excluded from ordinary memory-source ingestion.
11. Missing `MEMORY.md` is handled by creating one during dreaming.
12. Existing memory folder layouts are preserved; no required taxonomy is forced.

Suggested integration-style test

Create a temporary character memory folder with:

```text
memory/
  daily/2026-04.md
  shore-notes.md
  MEMORY.md
```

Put duplicate/stale/raw notes in `daily/2026-04.md`, such as:

```md
# Daily April Notes

- Trevor wants Shore memory to use MEMORY.md as an index.
- Trevor wants Shore memory to use MEMORY.md as an index.
- Old recap block should be replaced.
- The old main branch memory design benchmarked badly.
```

Use a mock LLM/tool-loop response that:

* reads/list/searches files
* writes/edits `shore-notes.md` with the durable design direction
* rewrites `MEMORY.md` as an index pointing to `shore-notes.md` and `daily/2026-04.md`
* appends `DREAMS.md` with an audit entry

Assert:

* `MEMORY.md` is index-shaped, not a fact dump.
* duplicate daily facts are consolidated into the durable file or noted as sorted.
* `DREAMS.md` records the files changed.
* no protected prompt files were modified.
* compaction remains uninvolved.

Expected final behavior

After this task:

* Normal prompts use `workspace/memory/MEMORY.md` as `<memory_index>`.
* Compaction preserves conversation material into markdown files and archives old turns.
* Dreaming is the memory librarian:

  * it uses an LLM/tool loop
  * it inspects existing memory files
  * it reorganizes/dedupes/sorts long-term memory
  * it updates `MEMORY.md` as an index/map
  * it records its actions in `DREAMS.md`
* The deterministic Light/REM/Deep scoring pass is no longer the primary dreaming implementation.
