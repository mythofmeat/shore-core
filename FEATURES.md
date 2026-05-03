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

## Config Hot Reload

The daemon watches Shore config inputs and reloads runtime config automatically
after edits. Hot reload covers the global config, included TOML files,
`conf.d/*.toml`, `.env`, per-character `config.toml` overlays, model catalogs,
behavior/tool settings, memory settings, autonomy config, and character
discovery.

If a changed config file is invalid, the daemon keeps using the previous valid
runtime config and logs the reload failure. Startup-owned settings such as the
daemon listener, Matrix supervision, notification backend, TTS connection, and
startup diagnostics still require a daemon restart.

Hot reload does not watch `characters/<Character>/workspace/**`; protected
prompt files and markdown memory keep their explicit compaction/reload
activation boundary.

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

Current LLM-facing memory surfaces:

| Surface | Purpose |
| --- | --- |
| Workspace `read`, `list_files`, `search` on `memory/...` | inspect markdown memory files when memory read access is enabled |
| Workspace `write`, `edit` on `memory/...` | update markdown memory files when memory write access is enabled |
| `search_history` | search active and compacted conversation transcripts |
| CLI/MCP memory commands | user/developer natural-language memory query surfaces |

There are no separate LLM-facing `memory_read`, `memory_write`,
`memory_search`, or `memory_list` tools on this branch.

`workspace/MEMORY.md` is the canonical memory index. Chat uses the
snapshot under `active_prompt/MEMORY.md`, so edits to the index only become
prompt-active after compaction/reload, matching the protected prompt files.
It is a concise index of memory files, recently updated files, and
still-relevant conversational throughlines; it is not the character definition,
user profile, standing behavior, tool guide, or heartbeat guide.

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
- `MEMORY.md` snapshot from `workspace/MEMORY.md`

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

When a heartbeat tick exhausts its tool-use budget without naturally finishing, the daemon appends a wrap-up nudge as a final user message, asking the character to record any unfinished work into `HEARTBEAT.md` and respond `HEARTBEAT_OK` (or send a final `<sendMessage>`). The model gets `wrap_up_grace_rounds` additional tool rounds to do that wrap-up before the loop hard-stops. The next heartbeat reads the updated `HEARTBEAT.md` from the start of its prompt, so notes left for future-self are always visible to the next session.

The heartbeat event log is persisted at `$XDG_DATA_HOME/shore/<Character>/heartbeat.jsonl` and is reloaded on daemon start, so tick decisions, autonomous messages, and dormancy transitions remain inspectable across restarts. Disk writes batch on the autonomy tick cadence (every ~30s) and on graceful shutdown.

`shore status` surfaces the autonomy block — heartbeat schedule (next wake, time since user, idle ticks, dormancy thresholds) and the most recent heartbeat events. `shore log --heartbeat` shows the full ring buffer.

## Dreaming

Dreaming is an opt-in scheduled AI librarian pass. When due, the character privately uses memory tools to list, read, search, write, and edit markdown memory files. Its job is to organize, dedupe, consolidate, and mark stale memory so future recall is easier. Dreaming may also edit the protected prompt files (`SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, `HEARTBEAT.md`); those edits stage through the active-prompt snapshot and take effect at the next compaction/reload boundary.

`workspace/MEMORY.md` is the canonical memory index and replaces the old recap/digest concept. Its active prompt snapshot refreshes at compaction, so dreaming can reorganize memory without changing the hot chat prefix immediately. It should point to useful files and throughlines without duplicating `USER.md` or `AGENTS.md`. Compaction is now responsible for adding the conversational throughline to `MEMORY.md` so the next conversation can pick up where the previous one left off; dreaming reorganizes the index later.

The dreams audit log lives at `$XDG_DATA_HOME/shore/<Character>/DREAMS.md` (data dir, not workspace) so it does not bleed into prompts or memory snapshots. The daemon writes the audit entry automatically after every dreaming and compaction pass; the model itself does not write `DREAMS.md`. Use `shore memory dreams [--limit N]` to inspect recent entries. Machine-readable dreaming state lives under `$XDG_DATA_HOME/shore/<Character>/dreams/`.

Generated dreaming output is excluded from ordinary memory-source ingestion, including legacy `.dreams/**`, `dreams.md`, `MEMORY.md`, and `memory/dreaming/**`.

## Tools

Tools are part of the character experience, not just an automation API.

Main tool groups:

- workspace `read`, `write`, `edit`, `delete`, `list_files`, `search`, and `exec`
- workspace `memory/...` access when memory gates allow it
- conversation transcript search via `search_history`
- web search and fetch
- image upload/vision and generated images via `generate_image`
- activity heatmap
- time and dice

`search` ranks workspace files using a hybrid of semantic similarity (vector
embeddings) and case-insensitive substring matching. Pass `mode: "lexical"`
for substring-only ordered by file recency, or `mode: "vector"` for pure
semantic similarity. Default is `hybrid`. When no embedder is configured or
the embedding model is unavailable, hybrid/vector requests fall back to
lexical and the response surfaces `semantic_unavailable` so the model knows
it didn't get the broader retrieval.

`exec` runs only allowlisted commands, does not invoke a shell, and now rejects path arguments outside the character workspace.

`delete` removes a workspace file by moving it into a timestamped folder under the character data directory's `trash/` subdirectory rather than erasing it. Prompt-visible files (SOUL.md, USER.md, AGENTS.md, TOOLS.md, HEARTBEAT.md, MEMORY.md) and directories cannot be deleted.

Memory access gates apply consistently: disabling memory blocks `memory/...`
paths through workspace tools, hides or disables history/memory read surfaces as
appropriate, and hides `exec` unless memory read/write are fully enabled.

Uploaded images may be persisted internally for history, replay, and UI display,
and their bytes are sent to capable models for vision. Uploaded attachment
filesystem paths are internal and are not exposed as something the character
should remember, reuse, or send later. The `generate_image` tool creates and
sends newly generated images.

Private conversations suppress memory access.

## Models And Providers

Shore resolves models from an effective catalog that merges three sources:

- Manual entries under `[chat.<provider>.<name>]` (`shore_config::models`).
- Provider-discovered models cached from `[providers.<name>]`'s
  `/v1/models` endpoint (Phase 5+).
- Hardcoded provider defaults (sdk + base_url) for well-known providers.

Saved sampler preferences (Phase 3) layer on top: character-scope wins
over global, both win over the static catalog. Visibility patterns on
the provider entry hide noisy upstream catalogs by default.

CLI surface (Phase 8):

```sh
shore model                         # list visible models (source-tagged)
shore model --all                   # also include hidden discovered models
shore model <name>                  # switch (alias / id / provider:id)
shore model --info [<name>]
shore model --reset

shore model setting                 # show effective sampler + scopes
shore model setting temperature 0.8
shore model setting reasoning_effort medium     # or "off" to clear
shore model setting --reset budget_tokens
shore model setting --global top_p 0.9          # write to global prefs

shore provider                      # list providers + key + cache status
shore provider models <name>        # discovered + static for one provider
shore provider models <name> --all  # also include hidden discovered models
shore provider refresh <name>       # re-fetch one provider's /v1/models
shore provider refresh              # refresh every discovery-enabled provider
```

The daemon also auto-refreshes any discovery-enabled provider whose cache
is missing or older than 24h, both at startup and on a 24h cadence while
running. Per-provider failures are logged and never block other providers
or the daemon itself.

Bash and Zsh completions stay static; Fish additionally completes provider
names for `shore provider models <TAB>` and `shore provider refresh <TAB>`
by querying the running daemon (silently empty when the daemon is down).

`shore reasoning ...` keeps working and is internally routed through the
shared sampler-preferences storage.

The TUI exposes the same surface via `:model`, `:model all`, `:provider`,
`:provider refresh <name>`, and `:setting <key> <value>`.

Hidden discovered models cannot be selected by ambiguous name. Pass
`--all` (CLI) or `:model all <name>` (TUI) to opt in for one call, or
edit the provider's `discovery.ignore` rules to opt in permanently.

## Clients

All clients connect to the daemon:

- `shore` — CLI and scripting surface
- `shore-tui` — terminal conversation UI
- `shore-gui` — Tauri GUI
- `shore-matrix` — Matrix bridge
- `shore-mcp` — development/debug MCP bridge

No client owns authoritative character state.

Streaming generation output (`StreamStart`/`StreamChunk`/`StreamEnd`) fans out to the issuing client and to the most recent client to send a real user message for that character (per-character lease, 60-minute idle expiry). So if you chat with a character on Matrix and then trigger a `:regen` from the TUI, the Matrix room sees the regenerated stream too. Non-streaming command output (`:status`, `:model`, …) stays with the issuing client.

## Matrix

The Matrix bridge can connect Shore characters to Matrix rooms. Embedded homeserver support is built around conduwuit-compatible servers, with external homeservers also supported.

Messages prefixed with `!` are commands. The bridge mirrors the TUI's slash-command translation (`!regen`, `!cancel`, `!status`, `!character`, `!model`, `!provider`, `!setting`, `!memory`, `!compact`, `!delete`, `!edit`, `!sys`, `!reasoning`, `!speak`), plus `!bind [character]` for room↔character binding and `!help` for an in-room reference. Unknown `!cmd args` is forwarded to the daemon as a generic command so handlers without a TUI shortcut (`!log`, `!heartbeat_log`, `!model_info`, `!diagnostics`, etc.) still work.

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
