# OpenClawify Patch Notes

These notes explain what changed on `breaking/openclawify`, why it matters, and what users need to do differently.

## Short Version

Shore's memory and character self-maintenance model changed from hidden runtime memory machinery to an OpenClaw-like workspace:

- character prompt files are markdown files in `workspace/`
- long-term memory is markdown under `workspace/memory/`
- protected self-edits are staged until compaction/reload
- heartbeat/autonomy is a private scheduled turn with no forced memory write
- old memory shell/collation/reindex/purge flows are gone from the normal user path

This is a big behavioral cleanup, not just a rename.

## Character Layout Changed

Old character layouts still migrate, but the current layout is:

```text
$XDG_CONFIG_HOME/shore/characters/<Character>/workspace/
  SOUL.md
  USER.md
  AGENTS.md
  TOOLS.md
  HEARTBEAT.md
  memory/
```

Mapping from old files:

| Old | New |
| --- | --- |
| `character.md` | `workspace/SOUL.md` |
| `user.md` | `workspace/USER.md` |
| `prompts/system.md` | `workspace/AGENTS.md` |

What to do:

1. Let Shore start once so migration can seed the workspace.
2. Review the generated workspace files.
3. Make future character/persona edits in `workspace/*.md`, not the old root files.

## Protected Self-Edits Are Deferred

The character can edit:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`

But those edits do not immediately change the active prompt. They are queued in:

```text
$XDG_DATA_HOME/shore/<Character>/deferred_edits.jsonl
```

They activate when compaction/reload refreshes:

```text
$XDG_DATA_HOME/shore/<Character>/active_prompt/
```

What to do:

- If a character edits its identity or instructions, expect the change to take effect after compaction/reload.
- Use status/character info surfaces to inspect pending deferred edits.
- Treat this as intentional cache protection, not a stale-file bug.

## Memory Is Markdown Now

Runtime memory now lives at:

```text
$XDG_CONFIG_HOME/shore/characters/<Character>/workspace/memory/
```

The character can inspect and maintain it with:

- `memory_read`
- `memory_write`
- `memory_search`
- `memory_list`
- workspace tools using `memory/...`

The CLI and MCP still provide a natural-language memory query surface, but the
LLM-facing memory tools are the granular markdown operations above.

What to do:

- Inspect memory directly in the filesystem.
- Prefer editing markdown files over trying to repair a DB.
- Organize memory with folders and headings when useful.

## Existing SQLite Memory Needs Exporting

The old SQLite/vector memory stack is not the runtime source of truth anymore.

Use:

```sh
scripts/migrate-memory.py
```

What to do:

1. Back up your config/data directories.
2. Run the migration script against your old memory DB.
3. Review the generated files under `workspace/memory/`.
4. Start Shore and run a memory query to verify the character can find them.

The old DB is not the normal runtime target after this branch.

## Compaction Changed

Compaction now:

- summarizes old conversation turns into markdown memory
- writes or updates files under `workspace/memory/`
- writes `active_prompt/RECENT_MEMORY.md`
- archives old conversation segments
- activates deferred protected prompt edits

Manual command:

```sh
shore memory compact
```

What to do:

- Use compaction as the normal boundary for memory consolidation and self-edit activation.
- Expect memory output to be human-readable markdown files.

## Removed Or Superseded Memory Workflows

The following old workflows are no longer the normal interface:

- interactive memory shell
- memory collation command/pipeline
- memory reindex as a required maintenance step
- memory purge as the main deletion flow
- passive RAG prompt injection
- hidden vector store as memory source of truth

What to do instead:

- Use markdown files directly.
- Use `memory_search` / `memory_read` / `memory_write`.
- Let compaction update memory during normal operation.

## Heartbeat Replaces Interiority Naming

Autonomous private ticks are now heartbeat ticks.

Config shape:

```toml
[behavior.autonomy]
enabled = true

[behavior.autonomy.heartbeat]
enabled = true
fallback_heartbeat_interval = "1h"
dormant_after_heartbeat_turns = 3
dormant_after_idle_time = "48h"
minimum_heartbeat_latency = "1h"
max_tool_rounds = 12
```

What to do:

- Rename old `interiority` config references to `heartbeat`.
- Update scripts that call old debug/status names.
- Do not expect heartbeat to force recaps or daily notes.

## Dreaming Replaces Collation

Memory consolidation now lives in opt-in dreaming:

```text
workspace/memory/.dreams/
workspace/memory/DREAMS.md
workspace/memory/MEMORY.md
```

`DREAMS.md` is reviewable output. Durable promoted facts land in `MEMORY.md`.

What to do:

- Enable `[memory.dreaming]` when you want scheduled consolidation.
- Use `shore memory dream --status`, `--dry-run`, or `--force` to inspect or run it.
- Do not expect recaps to appear in `shore log`.

## Tool Access Is Stricter

Memory gates now apply consistently:

- `memory = false` disables all memory tools.
- private conversations hide memory tools.
- workspace access to `memory/...` is blocked when memory access is disabled.
- `exec` is hidden unless memory read/write access is fully enabled.

What to do:

- If a character cannot see `memory/...`, check tool toggles and private mode first.

## `exec` Is Sandboxed More Tightly

The `exec` tool still supports allowlisted workspace commands, but now rejects path-like arguments outside the character workspace.

Allowed example:

```text
cat notes/todo.md
rg tea memory/
```

Rejected examples:

```text
cat /etc/passwd
rg tea ../
git -C /tmp status
cargo --manifest-path=/tmp/Cargo.toml test
```

What to do:

- Keep command work inside the character workspace.
- Use file tools for exact reads/writes.
- Use narrower commands when a flag embeds an absolute path.

## Optional Hybrid Retrieval

Default retrieval is lexical over markdown. Hybrid retrieval is available when an embedding profile is configured:

```toml
[defaults]
embedding = "text-large"

[memory.retrieval]
mode = "auto"

[embedding.text-large]
provider = "openai"
model_id = "text-embedding-3-large"
api_key_env = "OPENAI_API_KEY"
```

The semantic index is rebuildable and non-authoritative. If embeddings are unavailable, Shore falls back to lexical search.

## Patch Checklist For Existing Users

1. Back up config and data directories.
2. Start Shore once to seed/migrate workspace files.
3. Review `workspace/SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, and `HEARTBEAT.md`.
4. Export old SQLite memory with `scripts/migrate-memory.py` if needed.
5. Review generated `workspace/memory/` markdown.
6. Rename old `interiority` config/scripts to `heartbeat`.
7. Remove old memory shell/collation/reindex expectations from personal workflows.
8. Run `shore config --check`.
9. Run a memory query and a short conversation smoke test.
10. Run `shore memory compact` once when you are ready to activate staged prompt edits.

## Why This Is Better

This branch moves Shore closer to the goals in `GOALS.md`:

- long-lived characters
- low hot-context pressure
- inspectable memory
- character-managed files
- explicit prompt-cache boundaries
- autonomy that can maintain continuity without spamming the user

It should be easier to understand, easier to back up, easier to debug, and much less haunted by stale hidden indexes.
