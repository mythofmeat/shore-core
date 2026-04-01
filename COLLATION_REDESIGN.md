# Collation Pipeline — Behavioral Redesign

## What collation does

Collation takes raw memory entries (produced by compaction) and refines them:
- **Merge** similar entries into consolidated ones
- **Split** overly broad entries into focused ones
- **Normalize** entity names (deduplicate "Bob" vs "Robert")
- **Decay** confidence on stale entries

## Current behavior (broken)

1. **One-shot processing**: The `collation_skip` table permanently marks entries as "done." Once an entry is processed, it is never reconsidered — even if new related entries arrive that should be merged with it.

2. **Single giant LLM prompt**: All candidate entries are dumped into one prompt. With hundreds of entries this blows the context window and produces poor results.

3. **Split-then-merge ordering**: Entries are split first, then merged. This means an entry gets split into pieces that would have been merged back together anyway.

4. **Broken merge metadata**: Merged entries copy timestamps from the first source only. Split entries record only one replacement ID in `superseded_by`.

5. **No protection for special entries**: Image entries (where `image_path` is the real content) and canonical entries (user-verified) can be mangled by merge/split.

6. **Missing timestamps**: 74% of active entries have empty `start_timestamp`/`end_timestamp`, propagated from V1 import through collation.

7. **Confidence decay runs once**: Like everything else, gated by the skip table, so decay is one-shot instead of continuous.

## Desired behavior

### Candidate selection
- Entries should be **periodically reconsidered**, not permanently skipped.
- When new entries arrive (from compaction), existing entries on related topics should be mergeable with them.
- Processing should be **incremental** — not all 600+ entries at once, but a configurable batch per run.
- The system needs to be smart about **which** entries to reconsider: prioritize entries that have never been collated, then entries whose neighborhood has changed (new entries on the same topic).

### Entry protection
- Image entries (`image_path` non-empty) must be excluded from merge/split.
- Canonical entries (`canonical = true`) must be excluded from merge/split.

### Phase ordering
- Merge first, then split. Reduces churn.

### Merge/split correctness
- Merged timestamps: `min(sources.start)`, `max(sources.end)`, skipping empty strings.
- Merged message counts: sum across sources.
- Split supersession: all replacement IDs stored in `superseded_by`.

### Clustering
- Before sending entries to the LLM, group them into small clusters (5-15) by semantic similarity using existing vector store embeddings.
- Each cluster gets its own focused LLM call instead of one giant prompt.
- Falls back to sequential chunking without a vector store.

### Timestamp backfill
- Entries with empty timestamps should be incrementally backfilled by walking the `source_entry_ids` ancestry chain.
- Fallback: use `created_at` for entries with no ancestry.

### Confidence decay
- Runs on every active entry every time, not gated by any skip mechanism.

### Purge
- `shore memory purge --older-than 30d` deletes superseded entries whose replacements are still active.
- Image entries are excluded (attachment files need separate handling).
- Each deletion logged to changelog before removal.

### Observability
- Every operation logged to changelog with entry IDs.
- `shore memory collate` command with formatted output showing what happened.
- `--full` flag for convergence mode (loop until stable, capped at N passes).
- `--limit` flag to override batch size for manual runs.

## Resolved design decisions (implemented)

### Candidate selection: TTL-based reconsideration

New entries (empty `collated_at`) are always candidates. Previously-collated entries are reconsidered when their TTL has expired (default 7 days since last `collated_at`).

Processing is bounded by `memory.collation.batch_limit` (default 10) to control LLM cost. Running `shore collate` repeatedly works through the backlog incrementally. CLI `--limit` overrides the config default for manual runs.

Only entries that were actually examined as candidates get their `collated_at` stamped — unexamined entries preserve their TTL clock, and newly created entries start with empty `collated_at` to be candidates next run.

The vector store is used for *clustering quality* (grouping candidates into semantically focused LLM prompts) but not for candidate *selection*.

### Collation model: separate config option

Collation gets its own `defaults.collation` model config, distinct from `defaults.memory_agent`. Collation is synthesis/judgment work (merge decisions, split decisions, entity normalization) rather than retrieval, and memory quality depends on these decisions being good.

Fallback chain: `defaults.collation` → `defaults.memory_agent` → `defaults.model` → first chat model.
