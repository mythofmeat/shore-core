# Architecture

Shore is a daemon-centered AI character engine. The daemon owns state; clients observe and send commands.

## Workspace Crates

| Crate | Role |
| --- | --- |
| `shore-protocol` | SWP wire types |
| `shore-config` | config loading, model catalog, character paths |
| `shore-client` | client connection/discovery helpers |
| `shore-daemon-server` | TCP server, registry, session routing |
| `shore-daemon` | engine, memory, autonomy, tools, generation |
| `shore-llm-client` | provider request/stream handling |
| `shore-ledger` | usage, pricing, Anthropic cache tracking |
| `shore-cli` | CLI client |
| `shore-tui` | terminal UI |
| `shore-matrix` | Matrix bridge |
| `shore-mcp` | development/debug MCP surface |
| `shore-test-harness` | integration harness and mock server |

## State Model

Authoritative state lives in the daemon:

- active character
- conversation log
- message alternatives
- generation lifecycle
- memory and compaction
- heartbeat/autonomy scheduling
- ledger/cost state
- tool execution

Clients do not fork state. They attach, receive snapshots/events, and send SWP messages.

## File Layout

Config:

```text
$XDG_CONFIG_HOME/shore/
  config.toml
  .env
  characters/<Character>/workspace/
    SOUL.md
    USER.md
    AGENTS.md
    TOOLS.md
    HEARTBEAT.md
    memory/
```

Data:

```text
$XDG_DATA_HOME/shore/<Character>/
  active.jsonl
  active_prompt/
    SOUL.md
    USER.md
    AGENTS.md
    TOOLS.md
    HEARTBEAT.md
  compaction.json
  segments/
  deferred_edits.jsonl
  memory_index.json
```

Ledger:

```text
$XDG_DATA_HOME/shore/ledger.db
```

## Prompt Assembly

Prompt assembly reads protected prompt files from `active_prompt/`, not directly from editable workspace files.
It reads the prompt-visible memory index directly from `workspace/memory/MEMORY.md`.

Normal chat uses:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `workspace/memory/MEMORY.md`
- current conversation messages
- capability/tool guidance

Heartbeat additionally uses `HEARTBEAT.md`.

This design makes character self-editing compatible with Anthropic prompt caching: a workspace edit does not mutate the prompt prefix until compaction/reload.

## Deferred Protected Edits

Protected files:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`

When a model writes or edits one of these through workspace tools:

1. the workspace file changes immediately
2. a path is appended to `deferred_edits.jsonl`
3. normal prompt assembly keeps using the old `active_prompt/` snapshot
4. compaction/reload refreshes `active_prompt/` and clears the queue

## Conversation Engine

Messages are stored in `active.jsonl`. Compaction archives older messages into segment files and retains a configured number of recent turns.

The generation flow:

1. receive SWP client message
2. append user message
3. assemble prompt from active snapshot and active log
4. stream LLM response
5. run tool loop if the provider returns tool calls
6. persist assistant/tool messages
7. emit final stream metadata after persistence
8. trigger compaction if thresholds require it

The final `StreamEnd` is intentionally emitted after persistence so immediate follow-up commands see durable state.

## Memory

Memory is markdown under `workspace/memory/`.

Components:

- `markdown_store.rs` — filesystem store
- `markdown_query.rs` — direct and LLM-assisted markdown Q&A
- `retrieval.rs` — lexical and optional hybrid ranking
- `compaction/` — conversation summarization and memory writes
- `deferred_edits.rs` — prompt snapshot activation boundary

The optional embedding index is a rebuildable cache at `memory_index.json`. It is not a memory database.

## Tools

Tool definitions live under `shore-daemon/src/tools/`.

Tool categories drive private-mode filtering. Memory gates are enforced at both the visible tool list and dispatch layer.

Workspace tools resolve paths under the character workspace. `exec`:

- parses argv with `shell_words`
- never invokes a shell
- requires an allowlisted executable name
- rejects executable paths
- rejects path-like arguments outside the workspace
- runs in the workspace or validated subdirectory

## Autonomy

Autonomy is implemented as heartbeat state plus an async manager.

Heartbeat ticks:

1. rebuild the latest prompt from disk
2. inject the active `HEARTBEAT.md` plus runtime affordances
3. run a bounded tool loop
4. extract optional user-facing message
5. schedule the next wake or fall back to the configured interval

Dormancy stops autonomous LLM calls until user engagement resumes.

Cache keepalive is separate from heartbeat. It exists to preserve Anthropic cache warmth, not to simulate character autonomy.

Dreaming is the scheduled memory consolidation path. When autonomy and `[memory.dreaming]` are enabled, a due sweep runs Light -> REM -> Deep: Light stages deduplicated candidate state in `workspace/memory/.dreams/`, REM records theme/reinforcement signals, and Deep applies scoring gates before rewriting `workspace/memory/MEMORY.md` as the prompt-visible memory index.

`workspace/memory/MEMORY.md` orients the character with a map of memory files, recently updated files, and still-relevant conversational throughlines. It should not duplicate the roles of `SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, or `HEARTBEAT.md`.

`workspace/memory/DREAMS.md` is a human Dream Diary only. It is safe to edit for review, but it is not long-term memory and is not re-ingested. Generated outputs under `.dreams/**`, `DREAMS.md`, `dreams.md`, `MEMORY.md`, and `memory/dreaming/**` are excluded from candidate source collection.

## LLM Provider Boundary

`shore-llm-client` owns provider-specific request construction, streaming, response parsing, retry behavior, and content block handling.

Upstream crates should test business logic with the test harness, but provider wire behavior should be verified with recorded or live provider responses.

## Matrix

`shore-matrix` bridges Matrix rooms to SWP messages. Embedded mode manages a conduwuit-compatible homeserver; external mode connects to an existing Matrix server.

Matrix is a client/bridge, not an alternate state store.

## MCP

`shore-mcp` is primarily for development and agent-driven verification. It defaults to an isolated test profile and only writes to the real profile when explicitly attached with write permission.

## Removed Runtime Architecture

These are no longer the normal runtime memory architecture:

- SQLite memory entries table
- LanceDB/vector store as authoritative memory
- passive RAG prompt injection
- separate collation pipeline
- interactive memory shell

SQLite is still used for the usage ledger and may appear in migration tooling/history.
