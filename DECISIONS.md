# Decisions

This file records current architectural decisions first. Older V1/V2 notes were stale and have been superseded by the OpenClawify branch; use git history for the full archaeology.

## 2026-05-01: Workspace `search` Is Hybrid (Semantic + Lexical) With Local-First Embeddings

The workspace `search` tool ranks files by combining semantic similarity (cosine
over embeddings) with case-insensitive substring matching. Default fusion is
0.45 lexical + 0.55 cosine; callers can request `mode: "lexical"` (existing
recency-ordered substring search) or `mode: "vector"` (pure cosine).

Embeddings come from a pluggable `Embedder` trait. The default implementation
runs locally via fastembed-rs (BGE-small, 384 dims, ~33MB) so the daemon stays
key-free and offline-capable. Hosted embedders (OpenAI-compatible) live behind
the same trait and are selected by an `[embedding.<profile>]` TOML entry with
`provider = "openai"` (or other compat shape).

Why:
- The lexical-only `search` missed paraphrased queries and forced the assistant
  to keep guessing literal substrings.
- A hosted-only design adds an API-key requirement to a path that should work
  out of the box; the MTEB gap between local BGE-* and hosted frontier models
  is small enough at workspace scale that top-K results match in practice.
- Augmenting (rather than replacing) lexical preserves the recency-ordered
  behavior that already worked, and keeps rollout reversible via the `mode`
  parameter without prompt churn.

Implementation:
- File-level embeddings with character-cap truncation (no chunking in v1) —
  matches the existing memory-retrieval pattern; revisit if dogfooding shows
  the cap losing the right paragraph.
- Persistent JSON index at `<character_data_dir>/workspace_index.json`,
  brute-force cosine over all entries (no ANN — workspace size is hundreds to
  thousands of files).
- Non-UTF8 / oversize files are recorded in the index with `embedded: false`
  and a skip reason, so reindex doesn't churn binaries.
- Model weights cache under `$XDG_CACHE_HOME/shore/models/`, shared across
  characters (they aren't character state).
- Process-wide embedder cache so a single LocalEmbedder is shared across
  requests, characters, and heartbeat ticks rather than reloaded per call.

## 2026-04-24: Markdown Memory Is Authoritative

Runtime long-term memory is ordinary markdown under:

```text
characters/{character}/workspace/memory/
```

SQLite/vector/RAG memory is not part of normal runtime memory. It is kept only where still needed for the ledger, migration, old history, or experiments.

Why:

- The user wants inspectable, git-diffable memory.
- Character self-maintenance should operate on files it can read and edit directly.
- A hidden authoritative index creates split-brain memory.

Tradeoff:

- Lexical markdown search is less magical than a dedicated vector DB.
- Hybrid retrieval may use embeddings, but only as a rebuildable ranking aid.

## 2026-04-24: Protected Prompt Files Activate At Compaction Boundaries

Protected workspace files:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`

The active prompt reads from `{data_dir}/{character}/active_prompt/`. Workspace edits queue `deferred_edits.jsonl` and activate when compaction/reload refreshes the snapshot.

Why:

- Character self-editing is a core goal.
- Anthropic cache stability is also a core goal.
- Immediate prompt mutation would cause invisible cache invalidations.

Tradeoff:

- A character may not see its own protected self-edit in the very next turn.
- Status surfaces need to make pending deferred edits visible.

## 2026-04-24: Heartbeat Replaces Interiority Naming

The autonomy primitive is called heartbeat.

Why:

- `GOALS.md` names heartbeat/autonomy as the intended model.
- The old “interiority” wording drifted across docs and config.
- Heartbeat better describes scheduled private ticks that may or may not message the user.

Tradeoff:

- Users with old configs/scripts must rename old interiority fields and commands.

## 2026-04-24: Heartbeat Does Not Force Memory Writes

Heartbeat is a scheduled private turn governed by `HEARTBEAT.md`. The runtime provides affordances such as `set_next_wake`, `<sendMessage>`, bounded tool rounds, and `HEARTBEAT_OK`, but it does not force a recap or write daily notes.

Why:

- Heartbeat should stay character-directed rather than hardcoded maintenance.
- Durable memory should come from explicit write-capable tool use or dreaming.
- `HEARTBEAT_OK` gives the model a cheap acknowledgement/drop path.

## 2026-04-24: Dreaming Is Scheduled Memory Consolidation

Dreaming is the opt-in consolidation path. It stages machine-readable state, writes human-reviewable reports to `DREAMS.md`, and rewrites `MEMORY.md` during Deep Sleep as the prompt-visible memory index. `MEMORY.md` points to memory files, recent updates, and current throughlines; durable notes live in ordinary markdown memory files. `DREAMS.md` is not memory and generated dreaming artifacts are excluded from later candidate ingestion.

## 2026-04-30: Dreaming Machine State Belongs In Data

Dreaming machine-readable JSON lives under `$XDG_DATA_HOME/shore/<Character>/dreams/`, not under `characters/<Character>/workspace/memory/`. Character memory workspaces are meant to be editable and git-manageable; scheduler timestamps, staged candidate JSON, and debug state are daemon-owned runtime data.

## 2026-04-24: Remove Separate Collation As A Runtime Requirement

Compaction and tool use maintain markdown directly. There is no separate required collation pass.

Why:

- OpenClaw-style memory maintenance is file-oriented and agent-curated.
- Separate collation created a second mental model and a pile of stale config/docs.

Tradeoff:

- Memory quality depends more on compaction prompts, file structure, and the character’s own maintenance.

## 2026-04-24: Workspace Tools Are First-Class

Characters can read, write, edit, and list workspace files. They can also access `memory/...` paths when memory access is enabled.

Why:

- A character with autonomy needs a real workspace for memory, projects, and self-maintenance.
- File tools make behavior inspectable and recoverable.

## 2026-04-24: `exec` Is Allowlisted And Argument-Sandboxed

`exec` remains available for search, build, and inspection commands, but it:

- never invokes a shell
- accepts only allowlisted executable names
- rejects executable paths
- rejects path-like arguments outside the character workspace

Why:

- The tool is useful for code/workspace inspection.
- The previous “allowlisted executable only” rule still allowed arguments like `/etc/passwd` or `git -C /tmp`.

Tradeoff:

- Some legitimate commands with path-like non-path arguments may be rejected. Use file tools or narrower commands instead.

## 2026-04-24: Matrix Is A Client Bridge, Not A Core State Store

Matrix exists for mobile/convenience access. Embedded homeserver support targets conduwuit-compatible servers; external homeservers remain supported.

Why:

- The daemon remains the state owner.
- Matrix is a transport/client surface.

## 2026-04-24: Live API Verification Is A Release Gate, Not A Unit-Test Requirement

Fast tests use deterministic harnesses and mock HTTP servers where appropriate. Live tests remain mandatory before a real release when provider behavior is in scope.

Why:

- Real provider calls cost money and require credentials.
- Provider wire behavior still needs real/recorded verification before shipping.

## Superseded Historical Decisions

The following concepts appeared in older docs and changelogs but are not current runtime architecture:

- authoritative SQLite memory DB
- LanceDB/vector store as memory source of truth
- passive RAG injection
- interactive memory shell
- separate collation pipeline
- Synapse-specific embedded Matrix wording
- `character.md` as the active character definition path
- compaction-generated prompt recap files
- `memories/` as a runtime memory directory
