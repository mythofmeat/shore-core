# Architecture

Shore is a daemon-centered AI character engine. The daemon owns state; clients observe and send commands.

## Workspace Layout

The main Rust workspace is grouped by ownership:

- `core/` — shared protocol, config, and SWP client crates
- `backend/` — daemon runtime plus backend support crates
- `clients/` — user-facing clients, including CLI, TUI, and Tauri GUI
- `bridges/` — external service bridges
- `dev/` — development tools and test harnesses

`clients/gui-godot/rust` is intentionally outside the root Cargo workspace because
it has Godot-specific tooling and produces a `shore_bridge` dynamic library.

## Workspace Crates

| Path | Crate | Role |
| --- | --- |
| `core/protocol` | `shore-protocol` | SWP wire types |
| `core/config` | `shore-config` | config loading, model catalog, character paths |
| `core/swp-client` | `shore-swp-client` | client connection/discovery helpers |
| `backend/swp-server` | `shore-swp-server` | TCP server, registry, session routing |
| `backend/daemon` | `shore-daemon` | engine, memory, autonomy, tools, generation |
| `backend/llm` | `shore-llm` | provider request/stream handling |
| `backend/ledger` | `shore-ledger` | usage, pricing, Anthropic cache tracking |
| `backend/diagnostics` | `shore-diagnostics` | shared diagnostic formatting |
| `clients/cli` | `shore-cli` | CLI client |
| `clients/tui` | `shore-tui` | terminal UI |
| `clients/gui/src-tauri` | `shore-gui` | Tauri desktop client |
| `bridges/matrix` | `shore-matrix` | Matrix bridge |
| `dev/mcp` | `shore-mcp` | development/debug MCP surface |
| `dev/test-harness` | `shore-test-harness` | integration harness and mock server |

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
    MEMORY.md
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

Prompt assembly reads prompt-visible files from `active_prompt/`, not directly
from editable workspace files. `active_prompt/MEMORY.md` is refreshed from
`workspace/MEMORY.md` at the same compaction/reload boundary as the
protected prompt files.

Normal chat uses:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `active_prompt/MEMORY.md`
- current conversation messages
- capability/tool guidance

Heartbeat additionally uses `HEARTBEAT.md`.

This design makes character self-editing and memory-index maintenance compatible with Anthropic prompt caching: a workspace edit does not mutate the prompt prefix until compaction/reload.

## Deferred Prompt Edits

Prompt-visible files:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`
- `workspace/MEMORY.md`

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

Tool definitions live under `backend/daemon/src/tools/`.

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

Dreaming is the scheduled memory librarian path. When autonomy and `[memory.dreaming]` are enabled, a due pass makes a private LLM call with memory workspace tools. The character lists, reads, searches, writes, and edits markdown memory files to organize durable notes, dedupe repeated material, separate long-term facts from daily/raw logs, and mark stale or superseded information.

`workspace/MEMORY.md` is the canonical index; `active_prompt/MEMORY.md` is the prompt-active copy. It orients the character with a map of memory files, recently updated files, and still-relevant conversational throughlines. It should not duplicate the roles of `SOUL.md`, `USER.md`, `AGENTS.md`, `TOOLS.md`, or `HEARTBEAT.md`.

Compaction captures and preserves older conversation material into ordinary markdown memory files. Dreaming reorganizes and collates those files afterward. `workspace/memory/DREAMS.md` is the human-readable audit diary for dreaming and compaction review, while `.dreams/` stores machine-readable staging/debug state. Generated outputs under `.dreams/**`, `DREAMS.md`, `dreams.md`, `MEMORY.md`, and `memory/dreaming/**` are excluded from ordinary memory-source ingestion.

## LLM Provider Boundary

`shore-llm` owns provider-specific request construction, streaming, response parsing, retry behavior, and content block handling.

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
