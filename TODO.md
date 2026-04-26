Goal

Status: Completed on 2026-04-26.

Fix Shore’s dreaming system so it behaves like an OpenClaw-style memory consolidation pipeline instead of a superficial keyword scan with “Light / REM / Deep” labels.

The desired behavior:

- Dreaming is an opt-in scheduled background memory consolidation sweep.
- Machine-facing dreaming state lives under `workspace/memory/.dreams/`.
- Human-readable review output lives in `workspace/memory/DREAMS.md`.
- Optional per-phase reports may live under `workspace/memory/dreaming/<phase>/YYYY-MM-DD.md`.
- Long-term memory promotion writes only to `workspace/memory/MEMORY.md`.
- Only the Deep phase may write durable long-term memory.
- Light and REM may stage, summarize, dedupe, score, and record signals, but must never write to `MEMORY.md`.
- `DREAMS.md` is a human review diary, not a source of promotion truth and not a memory source to re-ingest.

Context

Shore’s project goals explicitly call for OpenClaw-like markdown memory and dreaming:

- `GOALS.md`
  - Dreaming should be a daily self-limited cleanup/update/curation pass similar to OpenClaw.
  - Heartbeat/autonomy is character free time; dreaming is slower consolidation.
- `FEATURES.md`
  - Says dreaming stages candidates under `workspace/memory/.dreams/`, writes review output to `DREAMS.md`, and promotes qualified facts to `MEMORY.md`.
- `ARCHITECTURE.md`
  - Says scheduled dreaming should stage state in `.dreams/`, append reviewable notes to `DREAMS.md`, and promote qualified durable facts into `MEMORY.md`.

Current implementation problem:

- `shore-daemon/src/memory/dreaming.rs`
  - Currently does a simple markdown line scan.
  - Scores lines by keyword matches like `likes`, `prefers`, `important`, `project`, etc.
  - Writes sections called `Light`, `REM`, and `Deep`, but the phases are not actually separate responsibilities.
  - Promotes anything with `score >= 2` into `MEMORY.md`.
  - This is too shallow and does not match the OpenClaw-style model we want.

Relevant existing plumbing:

- `shore-config/src/app.rs`
  - `MemoryConfig`
  - `DreamingConfig`
  - `enabled`
  - `frequency`
  - `max_tool_rounds`
- `shore-daemon/src/memory/dreaming.rs`
  - `DreamState`
  - `DreamCandidate`
  - `DreamSweepResult`
  - `DreamStatus`
  - `dream_status`
  - `run_sweep`
  - `is_due`
- `shore-daemon/src/commands/state/memory.rs`
  - `memory_dream`
  - manual status/dry_run/force path
- `shore-daemon/src/autonomy/manager.rs`
  - scheduled dreaming hook is already conceptually present via the autonomy tick loop
- `shore-daemon/src/memory/markdown_store.rs`
  - path safety and markdown memory IO
- `shore-daemon/src/memory/retrieval.rs`
  - optional retrieval/indexing behavior, if useful
- tests near `dreaming.rs` and command/state tests

Implementation requirements

1. [x] Replace the superficial one-pass keyword sweep with a real phase-oriented sweep.

Implement a pipeline roughly like:

- Light phase:
  - Read candidate material from normal memory sources.
  - Good sources:
    - daily memory files, if present
    - curated markdown memory files
    - recent compacted memory notes
  - Exclude generated dreaming artifacts:
    - `.dreams/**`
    - `DREAMS.md`
    - `dreams.md`
    - `memory/dreaming/**`
  - Do not promote anything.
  - Dedupe obvious duplicate lines.
  - Create staged candidate records under `.dreams/`.
  - Record basic evidence/provenance:
    - source path
    - source line number if feasible
    - text/snippet
    - first seen time
    - last seen time
    - simple source category
  - Write a human-readable Light Sleep section to `DREAMS.md` and optionally `memory/dreaming/light/YYYY-MM-DD.md`.

- REM phase:
  - Work from staged candidates/signals.
  - Extract coarse recurring themes/reflections using deterministic heuristics for now.
  - No LLM required unless existing architecture already has a clean model path for background work.
  - Do not promote anything.
  - Record REM reinforcement signals for Deep scoring.
  - Write a human-readable REM Sleep section to `DREAMS.md` and optionally `memory/dreaming/rem/YYYY-MM-DD.md`.

- Deep phase:
  - Score staged candidates.
  - Apply threshold gates.
  - Re-check/re-hydrate source snippets before promotion where possible.
  - Skip stale/deleted source snippets.
  - Skip candidates already present or near-duplicate in `MEMORY.md`.
  - Promote only qualified durable entries to `MEMORY.md`.
  - Write promotion/rejection explanation to `DREAMS.md` and optionally `memory/dreaming/deep/YYYY-MM-DD.md`.

2. [x] Preserve Shore’s markdown-first philosophy.

Do not introduce a database for dreaming.

Use JSON/JSONL under `.dreams/` for machine state, for example:

- `.dreams/state.json`
- `.dreams/candidates-YYYYMMDD-HHMMSS.json`
- `.dreams/phase-signals-YYYYMMDD-HHMMSS.json`
- `.dreams/promotions-YYYYMMDD-HHMMSS.json`

Exact file names can differ, but keep them obvious, inspectable, and git-diffable.

3. [x] Make `DREAMS.md` explicitly human-review-only.

Change generated `DREAMS.md` content so it clearly says:

- This file is a Dream Diary / review log.
- It is not long-term memory.
- Durable memory lives in `MEMORY.md`.
- Machine state lives in `.dreams/`.
- Editing/deleting Dream Diary sections should not directly change durable memory.

Format new entries like:

# Dreams

This file is the human-readable Dream Diary for Shore’s memory consolidation system.

It is not long-term memory.
Durable facts belong in `MEMORY.md`.
Machine-facing dreaming state belongs in `.dreams/`.

## Dream Cycle — <timestamp>

### Light Sleep — Staging

- Sources reviewed
- Candidates staged
- Duplicates ignored
- No durable memory was written

### REM Sleep — Reflection

- Themes noticed
- Reinforcement signals
- No durable memory was written

### Deep Sleep — Promotion

Promoted to `MEMORY.md`:

- candidate text
  - score
  - evidence/source
  - gates passed

Rejected/deferred:

- candidate text
  - reason

### Notes for Review

- Safe to edit/delete for human review.
- Does not control promotion state.

4. [x] Add real scoring gates.

Do not rely only on “contains likes/prefers/project” style scoring.

A simple deterministic first pass is fine, but structure it like real scoring so it can later be improved.

Suggested candidate fields:

- `text`
- `source`
- `line`
- `source_kind`
- `first_seen_at`
- `last_seen_at`
- `recall_count`
- `unique_source_count`
- `unique_query_count` if available, otherwise 0/1
- `theme_hits`
- `recency_score`
- `durability_score`
- `specificity_score`
- `promotion_score`
- `gates`
- `promote`
- `decision_reason`

Suggested initial gates:

- minimum score, maybe `0.60`
- minimum source/evidence count, maybe `1` for now
- not generated from dreaming files
- not already present in `MEMORY.md`
- not too short
- not a heading
- not obviously transient

Because Shore may not yet have OpenClaw’s recall/query-diversity machinery, implement placeholders cleanly rather than pretending they exist. For example, `unique_query_count` can be optional or defaulted, but the data model should support it.

5. [x] Avoid re-ingestion loops.

Generated files must not become future candidate sources:

- `.dreams/**`
- `DREAMS.md`
- `dreams.md`
- `memory/dreaming/**`

`MEMORY.md` may be read for dedupe checks, but it should not be treated as a source of new candidates for promotion.

6. [x] Preserve dry-run behavior.

`run_sweep(..., dry_run = true, ...)` must not write:

- `.dreams/**`
- `DREAMS.md`
- `MEMORY.md`
- `memory/dreaming/**`

Dry-run should return a full preview structure:

- candidate count
- staged candidates
- REM themes
- would-promote entries
- rejected/deferred entries
- would-write paths

7. [x] Preserve existing command behavior.

The existing `memory_dream` command path should continue to work:

- status mode
- dry_run
- force
- not_due response

But its returned JSON should become more useful:

- phase summaries
- candidate count
- promoted count
- deferred/rejected count
- paths written
- dry-run flag

Do not break callers unnecessarily unless there is a good reason.

8. [x] Keep config minimal for now.

Do not overbuild config unless needed.

Existing config:

[memory.dreaming]
enabled = false
frequency = "0 3 * * *"
max_tool_rounds = 12

It is okay to add conservative scoring constants in code first. If adding config keys, keep defaults backward-compatible and update docs/tests.

9. [x] Update docs.

Update these docs to match the new behavior:

- `FEATURES.md`
- `ARCHITECTURE.md`
- maybe `GOALS.md` only if wording needs clarification
- any current docs that mention Dreaming/DREAMS.md

Docs should emphasize:

- `DREAMS.md` is review output, not memory.
- `.dreams/` is machine state.
- only Deep writes `MEMORY.md`.
- Light and REM never promote.
- generated dreaming output is excluded from future candidate ingestion.

10. [x] Tests

Add or update tests covering:

- dry run writes no files
- Light and REM do not write `MEMORY.md`
- Deep writes `MEMORY.md` only for qualified candidates
- generated `DREAMS.md` is not re-ingested as a candidate source
- `.dreams/**` is not re-ingested
- `memory/dreaming/**` is not re-ingested
- existing `MEMORY.md` entries prevent duplicate promotion
- status reports due/not due correctly
- invalid schedule still errors
- symlink/path escape protections continue to pass
- manual `memory_dream` command returns useful JSON

Important constraints

- Do not introduce a database.
- Do not invoke a shell.
- Do not weaken path safety.
- Do not make dreaming mutate protected prompt files.
- Do not cause cache invalidation outside existing compaction/reload boundaries.
- Do not make dreaming dependent on Anthropic-specific behavior.
- Do not require an LLM call for the first version unless there is already an obvious, safe abstraction to use.
- Keep machine output inspectable and deterministic enough for tests.

Risks and edge cases

- The current markdown store may include generated files unless explicitly filtered.
- `DREAMS.md` can grow forever; do not solve full rotation unless simple, but avoid making growth worse with huge dumps.
- Candidate text may contain markdown syntax; escaping/formatting should not corrupt the diary.
- Duplicate detection does not need to be perfect, but exact and normalized duplicate checks are required.
- A failed Deep write should not leave state claiming promotion succeeded.
- If partial writes happen, returned paths and state should make debugging easy.
- Existing tests for symlink escape in `dreaming.rs` must remain meaningful.
- The order of phases should be Light → REM → Deep.
- The OpenClaw docs list Light → Deep → REM in one table but describe sweep execution as light → REM → deep elsewhere; for Shore, use Light → REM → Deep because it matches the desired narrative: stage, reflect, promote.

Completion notes

- Implemented Light -> REM -> Deep pipeline in `shore-daemon/src/memory/dreaming.rs`.
- Added JSON machine state under `.dreams/`, human review diary entries in `DREAMS.md`, optional phase reports under `dreaming/<phase>/`, and Deep-only `MEMORY.md` promotion.
- Added deterministic scoring gates, source rehydration, duplicate checks against `MEMORY.md`, and generated-output source exclusions.
- Updated command JSON/CLI summaries, docs, and tests.

Suggested validation steps

Run:

cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

Also manually test with a temp character workspace:

1. Create memory files with:
   - one durable repeated preference
   - one short transient note
   - one existing MEMORY.md duplicate
   - an existing DREAMS.md entry that would look promotable if incorrectly ingested

2. Run:

shore memory dream --dry-run --force

Expected:

- no files written
- preview shows candidates
- DREAMS.md content is not considered a source
- duplicate MEMORY.md item is rejected/deferred

3. Run:

shore memory dream --force

Expected:

- `.dreams/` state/candidate files written
- `DREAMS.md` diary entry written
- optional phase reports written if implemented
- only qualified Deep candidates appended to `MEMORY.md`

4. Run again immediately.

Expected:

- either `not_due` without force, or no duplicate promotions with force

Implementation note

Prefer making the phase model explicit in code rather than hiding it inside one large function. For example:

- `run_sweep`
- `run_light_phase`
- `run_rem_phase`
- `run_deep_phase`
- `collect_candidate_sources`
- `is_generated_dreaming_path`
- `score_candidate`
- `append_dream_diary`
- `append_memory_promotions`

The end result should make it obvious from code and docs that Shore dreaming is a reviewable, OpenClaw-like memory consolidation process, not a keyword-based “dream journal” generator.

[1]: https://docs.openclaw.ai/it/concepts/dreaming?utm_source=chatgpt.com "Dreaming - OpenClaw"
