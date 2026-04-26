Goal

Status: Completed.

Completion summary:

- [x] Replaced beginning-of-line `search` excerpts with match-centered excerpts.
- [x] Kept the existing `search` tool name, input schema, dispatch route, and output fields.
- [x] Bounded excerpts to about 1,200 source characters, with leading/trailing `...` when clipped.
- [x] Preserved source casing in returned excerpts while keeping search matching case-insensitive.
- [x] Added focused tests for long single-line files, clipping markers, bounds, and source casing.

Improve the existing dev-branch workspace `search` tool so it behaves like a useful grep: each result must include the searched string in the excerpt, with enough surrounding context to be useful, plus the file path and line number for follow-up reading.

This is intentionally a small, practical fix. Do not implement embeddings, semantic search, archive parsing, or a new tool.

Context - Completed

We are on the `dev` branch of `mythofmeat/silvershore`.

The current workspace search implementation is in:

- `backend/daemon/src/tools/workspace.rs`

The tool registry/dispatch is in:

- `backend/daemon/src/tools/mod.rs`

Current behavior was bad for long single-line files such as JSONL chat exports. The search detected a line containing the query, but the excerpt was taken from the beginning of the line. For JSONL lines with metadata before the actual message text, the excerpt often did not show the matched query at all.

The existing `search` tool remains the single user-facing tool. Its schema and name are unchanged.

Suggested files/modules/symbols inspected - Completed

- [x] `backend/daemon/src/tools/workspace.rs`
  - [x] `handle_search`
  - [x] `excerpt_line`
  - [x] `SEARCH_EXCERPT_CHARS`
  - [x] existing search tests in the same file

- [x] `backend/daemon/src/tools/mod.rs`
  - [x] Confirmed `search` dispatch still routes to `workspace::handle_search`

Implementation requirements - Completed

1. [x] Replace the current beginning-of-line excerpt behavior with a match-centered excerpt.

   Completed behavior:
   - The excerpt includes the matched query text.
   - The excerpt includes context before and after the match.
   - The excerpt preserves original casing from the source line.
   - The excerpt remains reasonably bounded.

2. [x] Keep `search` behavior otherwise unchanged.

   Preserved:
   - Existing tool name: `search`
   - Existing input schema
   - Existing output fields:
     - `query`
     - `results`
     - `count`
     - `searched_files`
     - `skipped_binary_or_large`
   - Existing per-result fields:
     - `path`
     - `line`
     - `excerpt`

3. [x] Recommended excerpt shape.

   Completed behavior:
   - Total source excerpt size is around 1,200 characters.
   - Context is roughly split before and after the match, with unused room rebalanced to the available side.
   - Leading `...` is used when content was clipped before the excerpt.
   - Trailing `...` is used when content was clipped after the excerpt.

Validation steps - Completed

Run:

- [x] `cargo fmt --all --check`
- [x] `cargo test -p shore-daemon search_excerpt`
- [x] `cargo test -p shore-daemon`
- [x] `cargo test --workspace`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
