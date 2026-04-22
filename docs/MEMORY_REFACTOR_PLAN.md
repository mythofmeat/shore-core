# Shore Memory Refactor Plan

## Status: In planning

## Goals

1. Replace the opaque SQLite memory store with inspectable, git-diffable markdown files.
2. Give the assistant real filesystem tools (read, write, edit, exec) so it can interact with its own workspace and memory like OpenClaw does.
3. Make compaction an AI-curated memory update rather than a deterministic summarization pipeline.
4. Drop Shore's bespoke collation in favor of whatever OpenClaw / Letta do.
5. Fix the high-priority problem: the assistant not using its tools to find memories when they would be useful.
6. Support deferred character self-edits so the character can rewrite its own files without invalidating prompt caches mid-conversation.

## Non-goals

- Do not port OpenClaw's plugin ecosystem, canvas, A2UI, or Node.js runtime.
- Do not change SWP, the tool loop, streaming, or the engine architecture.
- Do not change how LLM providers are integrated.

---

## Phase 1: Workspace + Filesystem Tools

**Purpose:** Give the assistant a real workspace it can read and write, independent of memory.

### New tools

| Tool | Purpose | Scope |
|------|---------|-------|
| `read` | Read any file under `{character}/workspace/` or `{character}/memories/` | General FS |
| `write` | Write or overwrite a file under workspace or memories | General FS |
| `edit` | Replace text in an existing file (Claude Code style) | General FS |
| `exec` | Run a shell command against an allowlist | General FS |
| `list_files` | List files and directories under a path | General FS |

### Design notes
- All paths are relative to `{character}/workspace/` or `{character}/memories/`.
- `write` auto-creates parent directories.
- `edit` uses **Claude Code format**: `path`, `edits[]` where each edit has `old_string` and `new_string`. The old string must match exactly (including whitespace). Multiple edits can be applied sequentially.
- `exec` uses a configurable allowlist. Default is restrictive: `ls`, `cat`, `rg`, `git`, `find`, `head`, `tail`, `wc`. Deny always wins.
- Path traversal protection identical to scratchpad.
- Tool descriptions are copied **verbatim from OpenClaw** where possible (it's open source). We benchmark whether this improves tool use.
- A manifest file (`{data_dir}/{character}/.workspace_manifest.json`) tracks file mtimes and embedding states. It lives **outside** the workspace so the assistant never sees it.

### Bootstrapped files (deferred-edit targets)

The following files are "bootstrapped" into the workspace for the assistant to edit, but changes are **deferred** until compaction to avoid cache invalidation:

| File | Workspace path | Purpose |
|------|---------------|---------|
| `character.md` | `workspace/character.md` | Character self-description |
| `user.md` | `workspace/user.md` | User description from character's POV |
| `prompts/system.md` | `workspace/prompts/system.md` | System prompt overrides |

At startup, Shore copies these config files into the workspace. The assistant can `read` and `edit` them freely. Edits are queued and applied at the next compaction (when context resets anyway). This mirrors OpenClaw's approach of letting the agent edit its own bootstrap files.

### Open questions
- Should we expose a `grep` / `search_files` tool or rely on the RAG index for searching?

---

## Phase 2: Markdown Memory Store

**Purpose:** Replace SQLite `entries` table with markdown files in `{character}/memories/`.

### Directory layout

```
{character}/
  memories/
    README.md              # Character-curated index / manifest
    topics/
      gaming/
        doom-speedrunning.md
      preferences/
        food.md
      people/
        ren.md
    sessions/
      2026-04-22_143022.md # Per-session raw dump (optional, for audit)
```

### File format

No YAML frontmatter. No structured metadata. Pure markdown:

```markdown
# Doom Speedrunning

- Ren plays UV-Max on Plutonia. Likes tight ammo balance.
- Mentioned this on 2026-04-21 during a conversation about retro games.
- His PB on MAP01 is 1:42.
```

The assistant decides the structure: headings, bullet points, nested folders, filenames.
We trust the model to organize. If it makes a mess, it can clean it up with its own tools.

### Indexing

- **RAG / vector search:** Index the full text of every `.md` file in `memories/`. When a file changes, re-embed it asynchronously.
- **BM25:** Index the same text for keyword search.
- **Search result format:** Return file path + relevant excerpt + score. The assistant can then `read` the full file if it wants more context.

### What happens to existing SQLite data

- `entries` table is deprecated but not dropped immediately.
- A one-time migration script dumps all existing entries to `{character}/memories/migrated/`.
- The memory agent stops querying SQLite `entries` and starts reading files.
- SQLite is eventually retained only for:
  - `flags` (issue tracking — low value, may be dropped)
  - `changelog` (audit trail — could become a markdown file too)
  - Vector store metadata (file paths, embedding IDs)

---

## Phase 3: AI-Curated Compaction

**Purpose:** Replace the deterministic compaction pipeline with a prompt that asks the assistant to update its own memory files.

### Current behavior (to replace)

1. Conversation grows.
2. Idle timer fires.
3. Shore calls a cheap model with a rigid prompt: "Summarize these messages into entries."
4. Structured entries are INSERTed into SQLite.

### New behavior

1. Conversation grows.
2. Idle timer fires (or user runs `shore memory compact`).
3. Shore sends a special system message: the assistant is in "memory mode." It has read access to the conversation history and its existing memory files. It is asked to:
   - Review the recent conversation.
   - Update existing memory files (edit, append, reorganize).
   - Create new files if new topics emerged.
   - Delete or merge files that are redundant.
   - Optionally write a brief `README.md` update if the overall structure changed.
4. The assistant uses its normal tools (`read`, `write`, `edit`) to make changes.
5. After the assistant finishes, Shore resets the conversation context (cache invalidation is now expected and natural).

### Key design decisions

- The assistant is explicitly told: "You are updating your own long-term memory. Be concise. Prefer updating existing files over creating new ones. Use clear filenames."
- Compaction is now a **tool-use turn** like any other, not a separate pipeline. This means the assistant's existing tool-calling behavior applies.
- The conversation context is truncated or reset after compaction. The assistant knows this and should write a good recap.
- This naturally solves the "character self-edit" problem: the character edits its own files during compaction, and the cache reset happens right after.

### Risk: cost

Old compaction used a cheap model with a rigid prompt. New compaction could use the main model because it's doing tool-use reasoning. Mitigation:
- Use the `compaction` model slot (already exists in config) which defaults to a cheap model.
- If the cheap model can't handle tool-use well, we may need to default compaction to the main model. Accept the cost for correctness.

---

## Phase 4: Drop Collation, Adopt OpenClaw/Letta Approach

**Purpose:** Replace Shore's 5-phase collation with whatever OpenClaw / Letta do for memory maintenance.

### Current behavior (to drop)

Shore collation: timestamp backfill → collate (merge) → tidy (split) → normalize entities → confidence decay. This runs automatically or on `shore memory collate`.

### New behavior

TBD. We need to research exactly what OpenClaw and Letta do.

**Hypothesis:** OpenClaw doesn't have a separate "collation" concept. Its memory maintenance is either:
- Implicit in compaction (the model decides to merge while updating files).
- Or a periodic "clean up your workspace" prompt.
- Or it doesn't do explicit maintenance, trusting the model to organize well.

**Action item:** Research OpenClaw's memory maintenance. Read their docs, prompt templates, or source if available. Same for Letta.

**Fallback:** If OpenClaw doesn't have a distinct collation phase, we simply drop it. The assistant is responsible for keeping its memory tidy during compaction. If memory grows too large, RAG/BM25 ranking handles it.

---

## Phase 5: Fix Tool Use for Memory Retrieval

**Purpose:** The assistant should proactively search and read memory when it would be useful.

### Problem statement

The assistant often fails to call `memory` (or the future `search_memories` / `read`) before answering. It hallucinates facts that are in its memory files, or it says "I think we talked about this" instead of searching.

### Hypotheses

1. **Tool description is too abstract.** The `memory` tool description says "query or update your memory database." That's vague. OpenClaw likely has more specific, compelling descriptions.
2. **Tool is too powerful / too opaque.** A single `memory` tool that both searches and saves is confusing. Splitting into `search_memories` and `save_memory` might help.
3. **System prompt doesn't prime retrieval.** The system prompt may not explicitly tell the model to search before guessing.
4. **Search result quality is poor.** If RAG returns garbage, the model learns not to trust it.

### Experiments to run

1. **Copy OpenClaw's memory tool descriptions verbatim.** Compare before/after.
2. **Split the tool:** `search_memories(query)` returns excerpts; `read_memory(path)` returns full file; `save_memory(path, content)` writes. This mirrors OpenClaw's explicit filesystem model.
3. **Add a retrieval reminder to the system prompt:** "Before making a factual claim about the user or past conversations, search your memories."
4. **Add a synthetic "memory check" tool call in the prompt examples.** Include a few-shot example where the model searches before answering.

### Success metric

In a benchmark conversation where the user references a past fact, the assistant should call a memory search tool ≥80% of the time before answering.

---

## Phase 6: Deferred Character Self-Edits

**Purpose:** Let the character edit its own `character.md`, `user.md`, and prompt files without breaking the prompt cache mid-conversation.

### Problem

If the assistant says "I should update my character.md to reflect that Ren likes jazz" and immediately writes the file, the next request ships the same cache markers but different prompt bytes → cache invalidation → full input price.

### Solution: deferred edit queue

1. During a conversation turn, if the assistant edits a "protected" file (`character.md`, `user.md`, `prompts/*.md`), the edit is not applied immediately.
2. Instead, the edit is queued in `{character}/deferred_edits.jsonl`.
3. At the next compaction (which already resets context), the queued edits are applied *before* the compaction prompt runs.
4. The assistant is informed that its edit has been deferred until the next memory update.

### Edge cases

- Multiple edits to the same file: apply in order, last write wins per section.
- Edits during compaction itself: apply immediately (compaction is the reset boundary).
- User manual edits: if the user edits `character.md` by hand, invalidate the deferred queue for that file (user wins).

---

## Phase 7: Migration from SQLite

### One-time migration script

```sh
shore memory migrate
```

1. Reads every row from `entries` table.
2. Writes a markdown file per entry to `{character}/memories/migrated/{id}.md`.
3. Content is the `summary_text` as markdown body.
4. Filename is derived from `topic_key` or `topic_tags` if available, else `migrated_{id}.md`.
5. After migration, mark the migration as complete in a sentinel file.

### Rollback plan

Keep the SQLite DB file. Don't delete it. If the markdown experiment fails, we can revert to SQLite reads. (But the goal is to eventually drop it.)

---

## Open Questions

1. **How does OpenClaw handle memory maintenance?** Need to research their prompts or source.
2. **Should we keep any SQLite at all?** Vector store metadata could be in SQLite or a simple JSON manifest.
3. **How do we handle large memory files?** If the assistant writes a 10,000-line `ren.md`, embedding the whole thing is fine, but returning it in RAG might be noisy. We may need chunking.
4. **What is the right `edit` tool format?** Find-and-replace? Unified diff? OpenClaw's `apply_patch`?
5. **Should `exec` require approval?** OpenClaw has configurable exec approvals. We probably want the same.

---

## Success Criteria

- [ ] `shore send` → assistant can `read` and `write` files in its workspace.
- [ ] `shore memory compact` → assistant reviews conversation and updates its own `.md` files.
- [ ] `shore memory search "doom"` → returns excerpts from markdown files.
- [ ] No SQLite `entries` table reads during normal operation.
- [ ] Character self-edits to `character.md` are deferred and applied at compaction.
- [ ] Assistant uses memory tools in ≥80% of relevant turns (benchmarked).

---

## Current Phase

**Phase 2: Markdown Memory Store — IN PROGRESS**

Phase 1 is complete and verified:
- `read`, `write`, `edit`, `list_files`, `exec` tools implemented
- All unit tests pass (14 workspace tests, 27 tools module tests)
- Live MCP verification passed with real OpenRouter Haiku LLM

Phase 2 infrastructure complete:
- `MarkdownMemoryStore` wired into `ToolContext` (handler, autonomy, tests)
- New memory tools implemented and registered:
  - `memory_read`, `memory_write`, `memory_search`, `memory_list`
- Compaction writes markdown files to `memories/compacted/` in addition to SQLite
- All unit tests pass (25 total tools, 700+ daemon tests)

**Still open within Phase 2:**
- Memory agent still reads from SQLite (transition path — will be addressed in Phase 5)
- Vector store / RAG does not yet index markdown files

Next action: Phase 3 — AI-curated compaction, or Phase 5 — split memory tool and improve retrieval.
