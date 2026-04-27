# Features

This document describes the current user-visible behavior of Shore. `GOALS.md` remains the source of truth for why the project exists.

## Characters

A character is a persistent persona with its own workspace, prompt files, memory, conversation log, and autonomy state.

Current workspace layout:

```text
$XDG_CONFIG_HOME/shore/characters/<Character>/workspace/
  SOUL.md       # character identity and long-lived self-definition
  USER.md       # what this character knows about the user
  AGENTS.md     # system/developer-style operating guidance
  TOOLS.md      # tool-use guidance
  HEARTBEAT.md  # heartbeat-only guidance
  memory/       # markdown long-term memory
```

Legacy files are migrated on first load:

| Legacy file | Workspace file |
| --- | --- |
| `character.md` | `workspace/SOUL.md` |
| `user.md` | `workspace/USER.md` |
| `prompts/system.md` | `workspace/AGENTS.md` |

The character may edit these files through workspace tools. Edits to protected prompt files are staged and only become prompt-active after compaction/reload so cache invalidation is explicit.

## Conversations

The daemon owns the authoritative conversation log. Clients are views and command senders.

Useful CLI commands:

```sh
shore send "hello"
shore send -i ./image.png "what is this?"
shore regen --guidance "try that more gently"
shore log
shore log edit last "replacement text"
shore log delete -1
shore character
shore character Alice
shore model
shore model claude-sonnet
```

Editing, deleting, and regenerating are supported because conversation repair is a core SillyTavern-style workflow.

## Memory

Runtime memory is markdown-first.

```text
$XDG_CONFIG_HOME/shore/characters/<Character>/workspace/memory/
```

The old runtime SQLite/vector/RAG memory stack is not the normal source of truth
on this branch. Historical migration helpers live in git history rather than
the active runtime surface.

<<<<<<< HEAD
```sh
scripts/migrate-memory.py
```

=======
>>>>>>> main
Current LLM-facing memory surfaces:

| Surface | Purpose |
| --- | --- |
| Workspace `read`, `list_files`, `search` on `memory/...` | inspect markdown memory files when memory read access is enabled |
| Workspace `write`, `edit` on `memory/...` | update markdown memory files when memory write access is enabled |
| `search_history` | search active and compacted conversation transcripts |
| CLI/MCP memory commands | user/developer natural-language memory query surfaces |

There are no separate LLM-facing `memory_read`, `memory_write`,
`memory_search`, or `memory_list` tools on this branch.

<<<<<<< HEAD
`workspace/memory/MEMORY.md` is the canonical memory index. Chat uses the
snapshot under `active_prompt/MEMORY.md`, so edits to the index only become
prompt-active after compaction/reload, matching the protected prompt files.
It is a concise index of memory files, recently updated files, and
still-relevant conversational throughlines; it is not the character definition,
user profile, standing behavior, tool guide, or heartbeat guide.
=======
`workspace/memory/MEMORY.md` is prompt-visible. It is a concise index of memory
files, recently updated files, and still-relevant conversational throughlines;
it is not the character definition, user profile, standing behavior, tool guide,
or heartbeat guide.
>>>>>>> main

Search is lexical by default. If an embedding profile is configured, retrieval can use a rebuildable hybrid semantic+lexical index. The index is a ranking aid only; markdown files remain authoritative.

## Compaction

Compaction turns older conversation turns into durable markdown memory and trims the hot conversation log. It writes:

- updated markdown files under `workspace/memory/`
- archived conversation segments under the character data directory

Compaction does not write `MEMORY.md`; dreaming maintains the canonical index.
Compaction is allowed to run on idle triggers, turn-count triggers, or
context-token safety triggers. It also activates staged protected prompt edits
and staged `MEMORY.md` index updates because that is already a cache-boundary
event.

Manual command:

```sh
shore memory compact
```

## Prompt Snapshots

Prompt-active files live under:

```text
$XDG_DATA_HOME/shore/<Character>/active_prompt/
```

Normal chat and heartbeat prompt assembly read from `active_prompt/`, not directly from editable workspace files. This keeps character self-editing compatible with Anthropic prompt caching.

Prompt-visible snapshot files:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`
- `MEMORY.md` snapshot from `workspace/memory/MEMORY.md`

`HEARTBEAT.md` is only injected into heartbeat ticks, not ordinary chat turns.

## Autonomy And Heartbeat

Autonomy is disabled by default. When enabled, the character may run private heartbeat ticks while active.

During a tick the character can:

- inspect conversation context
- read/search/write memory
- use tools
- schedule the next wake
- optionally send the user an autonomous message

The character becomes dormant after configured idle/tick limits and wakes when the user sends a message.

Heartbeat does not force a recap or write memory by itself. Durable notes are created only when the character uses a write-capable tool.

## Dreaming

Dreaming is an opt-in scheduled AI librarian pass. When due, the character privately uses memory tools to list, read, search, write, and edit markdown memory files. Its job is to organize, dedupe, consolidate, and mark stale memory so future recall is easier.

`workspace/memory/MEMORY.md` is the canonical memory index and replaces the old recap/digest concept. Its active prompt snapshot refreshes at compaction, so dreaming can reorganize memory without changing the hot chat prefix immediately. It should point to useful files and throughlines without duplicating `USER.md` or `AGENTS.md`. `workspace/memory/DREAMS.md` is the human-readable audit/review diary for each dreaming pass. Machine-readable state lives under `.dreams/`.

Generated dreaming output is excluded from ordinary memory-source ingestion, including `.dreams/**`, `DREAMS.md`, `dreams.md`, `MEMORY.md`, and `memory/dreaming/**`.

## Tools

Tools are part of the character experience, not just an automation API.

Main tool groups:

- workspace `read`, `write`, `edit`, `list_files`, `search`, and `exec`
- workspace `memory/...` access when memory gates allow it
- conversation transcript search via `search_history`
- web search and fetch
- image upload/vision and generated images via `generate_image`
- activity heatmap
- time and dice

`exec` runs only allowlisted commands, does not invoke a shell, and now rejects path arguments outside the character workspace.

Memory access gates apply consistently: disabling memory blocks `memory/...`
paths through workspace tools, hides or disables history/memory read surfaces as
appropriate, and hides `exec` unless memory read/write are fully enabled.

Uploaded images may be persisted internally for history, replay, and UI display,
and their bytes are sent to capable models for vision. Uploaded attachment
filesystem paths are internal and are not exposed as something the character
should remember, reuse, or send later. The `generate_image` tool creates and
sends newly generated images.

Private conversations suppress memory access.

## Clients

All clients connect to the daemon:

- `shore` — CLI and scripting surface
- `shore-tui` — terminal conversation UI
- `shore-gui` — Tauri GUI
- `shore-matrix` — Matrix bridge
- `shore-mcp` — development/debug MCP bridge

No client owns authoritative character state.

## Matrix

The Matrix bridge can connect Shore characters to Matrix rooms. Embedded homeserver support is built around conduwuit-compatible servers, with external homeservers also supported.

Matrix exists for convenience and mobile access; it is not a deeper protocol commitment.

## TTS

Shore supports on-demand and live text-to-speech through an OpenAI-compatible TTS provider.

```sh
shore speak "message id or text"
shore speak --live on
```

## Usage And Budget Awareness

LLM usage is recorded in the ledger at:

```text
$XDG_DATA_HOME/shore/ledger.db
```

`shore usage` exposes usage and cost breakdowns. Anthropic cache tracking is a first-class concern because unexpected cache invalidation has real cost.

## Remote Access

The daemon is localhost-only by default. Binding to a non-loopback address requires:

```toml
[daemon]
unsafe_allow_remote_access = true
allowed_hosts = ["100.64.0.2"]
```

`allowed_hosts` is an IP allowlist only. It is not authentication or encryption. Use Tailscale, WireGuard, or another trusted private overlay.
