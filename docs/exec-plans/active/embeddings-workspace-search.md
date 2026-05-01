# Embeddings-Backed Workspace Search

Status: implementation complete; awaiting dogfood validation
Owner: agent
Started: 2026-05-01
Implementation completed: 2026-05-01

## Goal

Augment the workspace `search` tool with vector retrieval so the assistant can
find relevant files by meaning, not just substring. The lexical path stays as a
fallback and as one signal in a hybrid ranker. Default embedder is local
(fastembed-rs / BGE-small) so the daemon stays offline-capable and key-free; a
hosted embedder remains available behind the same trait.

## Context

Existing infrastructure (already in tree, partially wired):

- `core/config/src/app.rs` defines `RetrievalMode` (`Auto` | `Lexical` |
  `Hybrid`) and `RetrievalConfig`.
- `backend/daemon/src/memory/retrieval.rs` implements `search_memory` with a
  hybrid lexical + cosine ranker, hash-based incremental indexing, and a
  JSON-on-disk index. **This function is never called.**
- `backend/llm/src/providers/mod.rs` exposes `embed()` routed to OpenAI only.
- `EmbeddingConfig` and `memory_index_path` already flow through
  `SharedToolContext` — the dependencies are wired, the consumer isn't.
- The `search` tool in `backend/daemon/src/tools/workspace.rs` is purely
  substring search with mtime-based ordering; it covers workspace and (when
  `include_memory` is true) the memory namespace.

Gap: lexical-only `search`; no local embedder; existing semantic plumbing only
covers memory markdown and is not invoked.

Relevant docs to update on completion:
[FEATURES.md](../../../FEATURES.md), [CONFIGURATION.md](../../../CONFIGURATION.md),
[ARCHITECTURE.md](../../../ARCHITECTURE.md),
[DECISIONS.md](../../../DECISIONS.md),
[CHANGELOG.md](../../../CHANGELOG.md).

## Scope

In:

- Index entire character workspace + memory namespace (single index).
- File-level embeddings with character-cap truncation (matches existing
  `MAX_EMBED_CHARS_PER_FILE = 4000` pattern).
- Local default (BGE-small via fastembed-rs); hosted (OpenAI/compat) behind
  same trait, selected by config.
- Hybrid ranking: combine normalized lexical score with cosine similarity.
- Augment, not replace: `search` tool gains a `mode` param
  (`hybrid` default | `lexical` | `vector`); existing JSON shape stays
  backwards-compatible (new fields, no removals).
- Skip non-UTF8 / oversize files (record them in index as
  `embedded: false, reason: ...` so reindex is idempotent).

Out:

- Multimodal embedders (user tracks roadmap elsewhere).
- ANN data structures (brute-force cosine is fine at workspace scale).
- SQLite-backed index (defer; JSON file is sufficient for v1 scale).
- Sub-file chunking (defer; track quality with tests, revisit if recall poor).

## Work Items

1. **Embedder trait in `shore-llm`.**
   - [x] Add `Embedder` trait: `async fn embed(&self, inputs: &[&str]) -> Result<Vec<Vec<f32>>>`,
     plus `dimensions()` and `model_id()`.
   - [x] Move existing OpenAI logic into `OpenAIEmbedder` impl.
   - [x] Keep `LlmClient::embed(...)` as a thin shim during transition; switch
     callers (`retrieval.rs`) to take `&dyn Embedder` instead.
   - [x] Unit test: trait dispatch round-trips a fake embedder.

2. **Local embedder: `LocalEmbedder` (fastembed-rs).**
   - [x] Add `fastembed` dep behind a `local-embeddings` Cargo feature
     (default-on for `shore-daemon`, off for crates that don't need it).
   - [x] Default model: `BAAI/bge-small-en-v1.5` (384 dims, ~33MB).
   - [x] Cache model files under
     `$XDG_CACHE_HOME/shore/models/<model_id>/` (not character data dir —
     models are shared across characters).
   - [x] First-run download with a single info-level log; surface a clear
     error if offline and model absent.
   - [x] Unit test: embed two semantically different inputs, assert vectors
     differ and `dimensions()` is 384.

3. **Embedder selection wiring.**
   - [x] Extend `[embedding.<profile>]` TOML schema with `provider = "local"`
     pointing to a fastembed model id, alongside the existing OpenAI shape.
   - [x] Default profile when none configured: local BGE-small.
   - [x] `resolve_embedding_config` returns a constructed `Box<dyn Embedder>`
     (or a builder), not a config struct that the call site has to interpret.
   - [x] Surface "embedder unavailable" reasons through the same
     `semantic_unavailable` field that `search_memory` already uses.

4. **Workspace index (extend `retrieval.rs`).**
   - [x] Generalize `search_memory` → `search_workspace`: walk the same set
     of paths the lexical `search` tool walks (workspace root, optionally
     memory) instead of only `MarkdownMemoryStore`.
   - [x] Reuse symlink-skip + size-cap + non-UTF8 handling from the lexical
     path so security parity is exact.
   - [x] Index entry gains `embedded: bool` and `reason: Option<String>` so
     skipped files don't churn on every search.
   - [x] Index path: `<character_data_dir>/workspace_index.json` (separate
     from existing `memory_index.json` to avoid format collisions during
     rollout; the memory-only path can be retired once parity is proven).

5. **Wire `search` tool to hybrid retrieval.**
   - [x] Add `mode` param to `search` tool def: enum default `hybrid`.
   - [x] When mode is `lexical`, behavior unchanged.
   - [x] When mode is `hybrid` or `vector`, compute lexical scores +
     cosine, fuse with the existing 0.45/0.55 weighting (revisit after
     dogfood), return path/line/excerpt the same way; line-level excerpts
     stay lexical-derived (vector hits still need a line excerpt — pick the
     line with the best fuzzy term overlap, fall back to the file's first
     non-blank line).
   - [x] Update tool description in `tool_defs()` and the
     `description_for_memory_access` variant.
   - [x] On embedder failure, fall back to lexical and surface `note` in the
     response.

6. **Tests.**
   - [x] Unit: hybrid promotes a semantic-only match above an unrelated
     lexical hit (analogue of the existing
     `hybrid_ranking_can_promote_semantic_match` test, at workspace scope).
   - [x] Integration: `cargo test -p shore-daemon tools::workspace::search`
     covering lexical-only, hybrid-with-mock-embedder, and embedder-failure
     fallback paths.
   - [x] Symlink + non-UTF8 + oversize files round-trip through index
     without crashing and remain marked `embedded: false`.
   - [x] Reindex incrementally: modifying one file embeds only that file
     (assert mock embedder call count).

7. **Docs.**
   - [x] `FEATURES.md`: describe hybrid `search` and the `mode` param.
   - [x] `CONFIGURATION.md`: local vs hosted embedder profiles, model cache
     location, default behavior when no profile is set.
   - [x] `ARCHITECTURE.md`: data flow from search call → Embedder →
     index → ranker.
   - [x] `DECISIONS.md`: local-first default, file-level embeddings, JSON
     index, augment-not-replace.
   - [x] `CHANGELOG.md`: user-visible "search now uses embeddings" line.

## Validation

- [x] `cargo fmt --all --check`
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo test --workspace`
- [x] `cargo test -p shore-daemon memory::retrieval`
- [x] `cargo test -p shore-daemon tools::workspace`
- [x] `python3 scripts/harness-check.py`
- [ ] Manual dogfood: run `search` on a real character workspace with a
  paraphrased query and confirm semantic hit appears in top-K.

## Decisions

- 2026-05-01: **Local default (BGE-small via fastembed-rs).** Daemon stays
  offline-capable and key-free; MTEB gap to hosted frontier is small at
  workspace scale (top-K is usually identical). Hosted remains available
  behind the same trait via config.
- 2026-05-01: **Augment, not replace.** Add `mode` param to `search`,
  default `hybrid`. Lexical recency-ordered behavior remains reachable so
  rollout is reversible without prompt churn.
- 2026-05-01: **File-level embeddings with char-cap truncation, no
  chunking in v1.** Matches existing `MAX_EMBED_CHARS_PER_FILE` pattern;
  chunking adds index complexity that may not move recall at this scale.
  Track via tests; revisit if dogfooding shows the cap losing the right
  paragraph.
- 2026-05-01: **JSON-on-disk index, brute-force cosine, no ANN.**
  Workspace size is hundreds–thousands of files; SQLite/HNSW are premature.
- 2026-05-01: **Skip non-UTF8 / oversize, record reason in index.**
  Multimodal is roadmap-tracked outside this plan; recording the skip
  reason keeps the index idempotent so reindex doesn't churn binaries.
- 2026-05-01: **Model cache under `$XDG_CACHE_HOME/shore/models/`,** not
  per-character data dir — embedding model weights are shared across
  characters and aren't character state.
- 2026-05-01: **Separate `workspace_index.json` from existing
  `memory_index.json`.** Avoids schema collision while the memory-only
  pipeline still exists; we can retire the memory-only path once the
  workspace index covers it.

## Handoff Notes

Implementation:
- `Embedder` trait lives in `backend/llm/src/embed/` with `OpenAIEmbedder`
  and (feature-gated) `LocalEmbedder` impls. A process-wide cache
  (`shore_llm::embed::cache_or_build`) keyed by `provider::model_id`
  ensures the local ONNX model is loaded once per process.
- `LlmClient::embed` was removed; nothing else used it. `providers::embed`
  is still the OpenAI-compat HTTP path used by `OpenAIEmbedder`.
- `backend/daemon/src/memory/workspace_index.rs` walks the workspace +
  memory with the same security rules as the lexical `search` tool,
  embeds text files, persists to
  `<character_data_dir>/workspace_index.json`. Non-UTF8 / oversize entries
  are stored with `embedded: false` so reindex is idempotent.
- `backend/daemon/src/memory/retrieval.rs` is now embedder-resolution only.
  The previous memory-only `search_memory` function (and its
  `MemoryIndex` / `IndexedEntry` types) was deleted because the
  workspace-wide hybrid path supersedes it; `dreaming.rs` continues to
  own its own use of `memory_index.json` for an unrelated purpose.
- Tool dispatch passes `ctx.embedder()` and a `workspace_index.json` path
  computed from `ctx.character_data_dir()`. Tests pass `None` to fall
  through to lexical behavior.

Deferred (not blockers, but flagged):
- Sub-file chunking. Currently each file embeds the first
  `MAX_EMBED_CHARS_PER_FILE` chars only. If dogfooding shows the cap
  losing the right paragraph, add chunking before reaching for ANN.
- Multimodal embedders for binary files. The index already records
  binaries with `embedded: false`; swapping in a multimodal embedder
  would only need a new `Embedder` impl + a branch in
  `workspace_index::hybrid_search` to embed binaries instead of skipping.

Validation gap:
- Manual dogfood against a real character workspace is the one item
  still open from the validation list. The first-call latency of the
  local model includes a ~30MB download into
  `$XDG_CACHE_HOME/shore/models/` (one-time per machine).
