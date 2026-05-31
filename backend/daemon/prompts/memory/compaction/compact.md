The conversation above is now complete and will be archived once you finish writing memory. Review it, then call your `write`/`edit` tools to persist anything important.

Your `MEMORY.md` index (already in your system prompt) maps your existing memory files. Use `list_files`, `read`, and `search` to check what's already there before writing, so you edit in place instead of creating near-duplicates.

Reminder:
- Only `MEMORY.md` (workspace root) and paths under `memory/` are writable.
- Prefer **edit**ing an existing file over creating a near-duplicate one.
- End with a brief plain-text summary (no tool call) when you're done.
- If you produce zero writes, the conversation will NOT be archived and the next compaction trigger will retry.
