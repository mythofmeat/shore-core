Goal

Status: Completed on 2026-04-26.

[x] Change Shore's memory architecture so `workspace/memory/MEMORY.md` becomes the prompt-visible memory index, replacing the old compaction-generated recap / `RECENT_MEMORY.md` behavior.

The new conceptual split is:

- [x] Compaction captures/preserves older conversation material into markdown memory notes.
- [x] Dreaming organizes/collates memory files and keeps `MEMORY.md` useful.
- [x] `MEMORY.md` orients the character: it is a map of memory files, recently updated files, and still-relevant conversational throughlines.

Context

Shore currently has fixed prompt files:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`

[x] These already cover character identity, user facts, standing behavior, tool guidance, and heartbeat-specific guidance. `MEMORY.md` now explicitly avoids duplicating those roles.

`MEMORY.md` lives under the character workspace memory directory:

```text
$XDG_CONFIG_HOME/shore/characters/<Character>/workspace/memory/MEMORY.md
```

Completion notes

- Prompt assembly now reads `workspace/memory/MEMORY.md` as a `memory_index` system block.
- Compaction now writes markdown memory notes only; it no longer generates or writes `RECENT_MEMORY.md` recaps.
- Compaction refuses writes to `MEMORY.md`, `DREAMS.md`, `.dreams/**`, and `dreaming/**` so dreaming owns the index/review outputs.
- Dreaming rewrites `MEMORY.md` as a prompt-visible index of memory files, recent updates, and scored throughlines.
- Docs and cache-test scripts now refer to the workspace memory index instead of `active_prompt/RECENT_MEMORY.md`.
