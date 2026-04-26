Goal

Change Shore’s memory architecture so `workspace/memory/MEMORY.md` becomes the prompt-visible memory index, replacing the old compaction-generated recap / `RECENT_MEMORY.md` behavior.

The new conceptual split is:

- Compaction captures/preserves older conversation material into markdown memory notes.
- Dreaming organizes/collates memory files and keeps `MEMORY.md` useful.
- `MEMORY.md` orients the character: it is a map of memory files, recently updated files, and still-relevant conversational throughlines.

Work on the `dev` branch.

Context

Shore currently has fixed prompt files:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`

These already cover character identity, user facts, standing behavior, tool guidance, and heartbeat-specific guidance. Do not duplicate those roles in `MEMORY.md`.

`MEMORY.md` should live under the character workspace memory directory:

```text
$XDG_CONFIG_HOME/shore/characters/<Character>/workspace/memory/MEMORY.md
