# Architecture

Shore is a daemon-centered AI character engine. The daemon owns state; clients
observe and send commands over SWP.

This file is the compact system manual: runtime shape, load-bearing invariants,
security boundaries, observability, and validation expectations.

## Workspace Crates

| Path | Crate | Role |
| --- | --- | --- |
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

`clients/gui-godot/rust` is intentionally outside the root Cargo workspace
because it has Godot-specific tooling and produces a `shore_bridge` dynamic
library.

## State Model

Authoritative state lives in the daemon:

- active character
- conversation log and message alternatives
- generation lifecycle
- memory and compaction state
- heartbeat/autonomy scheduling
- ledger/cost/cache state
- tool execution

Clients attach, receive snapshots/events, and send SWP messages. CLI, TUI, GUI,
Matrix, and MCP must not become alternate sources of character truth.

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
    MEMORY.md     # optional/generated prompt-visible index
    memory/       # markdown long-term memory
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
  deferred_edits.jsonl
  segments/
  dreams/
  workspace_index.json
```

Global data:

```text
$XDG_DATA_HOME/shore/ledger.db
$XDG_DATA_HOME/shore/cache_forensics.jsonl
```

## Runtime Flow

Generation:

1. Receive an SWP client message.
2. Append the user message to `active.jsonl`.
3. Assemble the prompt from `active_prompt/` and active conversation messages.
4. Stream the LLM response.
5. Run tool loops if the provider returns tool calls.
6. Persist assistant/tool messages.
7. Emit final stream metadata after persistence.
8. Trigger compaction if thresholds require it.

The final `StreamEnd` is emitted only after persistence, so immediate follow-up
commands see durable state. During tool use, clients may see intermediate
`StreamEnd(tool_use)` events; they should buffer one assistant turn across tool
phases.

Regeneration uses the same prompt assembly path, but the prompt view stops at
the last real user turn so the model does not see the response being
regenerated. The daemon does not rewrite `active.jsonl` until the replacement
response has completed; then it atomically replaces the assistant/tool tail and
stores the old and new visible assistant bodies as selectable alternate
responses on the active assistant message. Selecting a prior alternate rewrites
the active tail to that response and advances the history rewrite generation, so
stateful providers do not keep remembering the discarded active response.

### Claude Code Provider

Models with `sdk = "claude_code"` use the local `claude` CLI as a subprocess
transport instead of an HTTP API key. Shore still owns conversation state,
tools, memory, ledger rows, and persistence.

```text
shore-daemon
  ├─ SWP listener for clients
  ├─ HTTP listener /mcp/<session-id>
  │    └─ tools/list + tools/call -> daemon tool dispatch
  └─ shore-llm request
        └─ claude --print --input-format stream-json --mcp-config <daemon URL>
```

The daemon starts the HTTP listener when `[daemon.http].enabled = true` or when
the loaded chat catalog contains a `claude_code` model. Before a Claude Code
generation, the engine allocates an MCP session, injects `mcp_endpoint`,
`allowed_tools`, `session_id`, and `subprocess_key` into `provider_options`,
then dispatches to `shore-llm`. Tool calls happen inside the CLI's turn over
HTTP MCP; the daemon records them in a per-turn ledger and splices synthetic
`tool_use` and `tool_result` blocks into the assistant message before
persistence. Background tasks such as heartbeat, compaction, and dreaming use
the same callback session mechanism around their non-streaming `generate()`
calls.

Client streaming enables Claude Code's `--include-partial-messages` flag and
forwards parsed Shore text/thinking events as each partial `stream_event`
arrives. The final assistant event is still consumed for tool-use blocks and
turn completion, but completed text/thinking blocks are not re-emitted when
partials already covered them.

Claude Code CLI 2.1.128 does not deliver Anthropic-style base64 stream-json
image blocks to the model in live testing, and the official SDK documentation
currently says stream-json input is text-only. For current-turn image
attachments, Shore bridges the gap through a private per-session
`shore_attached_image` MCP tool: the stdin image block becomes a text pointer,
and the tool returns MCP image content from Shore's already encoded attachment
payload. This keeps image input inside the daemon's MCP permission boundary and
does not enable Claude Code's built-in filesystem `Read` tool.

`shore-llm` keeps a long-lived subprocess cache keyed by `subprocess_key` when
the daemon provides one, with fresh-spawn fallback for cold starts, dead
children, recipe changes, and subprocesses idle for at least one hour. The MCP
URL is stable per subprocess key while the daemon rotates the per-turn ledger
behind that session. The daemon holds a per-key MCP session lock before
dispatching to the provider, so concurrent turns for the same character cannot
rebind the stable URL to a newer tool context while an older CLI run is still in
flight. Claude Code reported
`total_cost_usd` is stored as would-be API cost for observability; it is not the
user's actual subscription spend.

When a cold start has prior Shore history, the provider writes that history into
Claude Code's native JSONL session format under `~/.claude/projects/<cwd-slug>/`
and starts the CLI with `--resume <session_id>`. That gives the CLI structured
conversation history after compaction, daemon restart, or subprocess death
without replaying old turns through stdin. If the history is empty, or
`provider_options.native_session_replay = false`, the provider falls back to the
older transcript-in-system-prompt path with `--no-session-persistence`.

The MCP listener is bearer-by-URL and loopback-only by default. A local process
that can read the `claude` subprocess command line can see the `--mcp-config`
URL and replay tool calls while that session is active, so the HTTP listener
must not be exposed casually. Non-loopback `[daemon.http].bind_addr` values
require `[daemon].unsafe_allow_remote_access = true`; `[daemon].allowed_hosts`
does not filter the HTTP MCP listener.

The provider writes system prompts to a temporary file and passes
`--system-prompt-file` to avoid putting prompt text in argv. This is an
undocumented Claude Code flag, so the ignored live tests and
`dev/test-harness/claude_code/run.sh` are the guardrail for CLI compatibility
across Claude Code upgrades.

Known non-parity with direct Anthropic/OpenRouter API providers is tracked in
`docs/claude-code-parity.md`.

## Config Runtime

The daemon loads config at startup and keeps a runtime copy in the message
handler, command context, autonomy manager, and character registry.

Manual `config_reset` and automatic hot reload use the same application path:
parse config, replace runtime config, invalidate merged per-character config
caches, rescan character discovery, update autonomy runtime config, and push
fresh snapshots to connected sessions.

The watcher covers config TOML inputs, `.env`, `conf.d/`, and per-character
`config.toml` overlays. It deliberately ignores
`characters/<Character>/workspace/**` so prompt and memory edits keep their
explicit compaction/reload boundary.

Startup-owned settings such as daemon listener, Matrix supervision,
notifications, TTS connection setup, and startup diagnostics require restart.

## Prompt And Cache

Prompt assembly reads prompt-visible files from `active_prompt/`, not directly
from editable workspace files.

Normal chat uses:

- active `SOUL.md`
- active `USER.md`
- active `AGENTS.md`
- active `TOOLS.md`
- active `MEMORY.md`
- current conversation messages
- stable capability/tool guidance

Heartbeat additionally uses active `HEARTBEAT.md` and heartbeat runtime
affordances.

Prompt-visible workspace files are:

- `SOUL.md`
- `USER.md`
- `AGENTS.md`
- `TOOLS.md`
- `HEARTBEAT.md`
- `MEMORY.md`

When a model writes or edits one of these files through workspace tools, the
workspace file changes immediately, but the path is queued in
`deferred_edits.jsonl`. Normal prompt assembly keeps using the old snapshot until
compaction/reload refreshes `active_prompt/` and clears the queue.

Unexpected Anthropic cache invalidation is a serious regression. Things that
should not bust cache include ordinary workspace edits, ordinary markdown memory
writes, tool loop bookkeeping, activity tracking, and image cache warmups.
Expected cache breakpoints include compaction/reload, activating staged prompt
edits, editing old conversation messages, changing model/provider/cache
settings, and changing prompt templates or tool definitions in code.

## Memory

Runtime long-term memory is markdown under:

```text
characters/<Character>/workspace/memory/**/*.md
```

Curated markdown files are authoritative. SQLite/vector/RAG memory is not part
of normal runtime memory. Optional semantic indexes are rebuildable ranking aids.

`MEMORY.md` lives at the workspace root and is prompt-visible through
`active_prompt/MEMORY.md`. It is a concise map of memory files, recent updates,
and conversational throughlines. It is not the character definition, user
profile, standing behavior, tool guide, or heartbeat guide.

Compaction turns older conversation material into durable markdown memory,
archives compacted messages into `segments/`, retains configured recent turns,
updates `MEMORY.md` with carry-forward throughlines, and activates deferred
prompt edits. Dreaming may later reorganize the memory files and `MEMORY.md`.

Dreaming is an opt-in scheduled AI librarian pass. When autonomy and
`[memory.dreaming]` are enabled, the character privately uses memory/workspace
tools to inspect, dedupe, consolidate, and mark stale or superseded memory.
Dreaming may edit prompt-visible files; those edits follow the same deferred
activation rule.

The dreams audit log lives at:

```text
$XDG_DATA_HOME/shore/<Character>/DREAMS.md
```

It is daemon-written and is not memory. Machine-readable dreaming state,
staged outputs, and legacy diagnostic reports live under
`$XDG_DATA_HOME/shore/<Character>/dreams/`. Legacy workspace artifacts under
`.dreams/**`, `dreams.md`, `MEMORY.md`, and `memory/dreaming/**` are excluded
from ordinary memory-source ingestion.

Search is lexical or hybrid semantic+lexical. The workspace-wide hybrid index is
stored at `<character_data_dir>/workspace_index.json`; markdown files remain
authoritative and the index can be deleted and rebuilt. Search/index walks the
whole workspace tree (including `memory/`) with configurable file-size, file
count, and total-byte limits.

## Tools And Security

Workspace tools operate inside `characters/<Character>/workspace/`.

Rules:

- Paths must stay inside the character workspace.
- Symlink and traversal escapes are bugs.
- Workspace file tools treat `memory/...` as a normal workspace subdirectory.
- Private conversations hide `search_history` and `exec`, and still suppress
  prompt-visible memory index injection.
- Prompt-visible files cannot be deleted and edits are deferred.

`exec` is intentionally narrow:

- command strings are parsed to argv and executed directly
- no shell is invoked
- executable names are allowlisted
- executable paths are rejected
- path-like arguments must stay inside the character workspace
- the command runs in the workspace or a validated subdirectory

Remote daemon access is explicit. Non-loopback binding requires:

```toml
[daemon]
unsafe_allow_remote_access = true
allowed_hosts = ["100.64.0.2"]
```

`allowed_hosts` is a source-IP allowlist only. It is not authentication or TLS.
Use a private overlay network such as Tailscale or WireGuard.

Provider keys come from environment variables or `.env` in the config directory.
Do not commit real keys, captured Authorization headers, or private profile
data.

## Autonomy

Autonomy is implemented as heartbeat state plus an async manager. It is disabled
by default.

Heartbeat ticks:

1. Rebuild the latest prompt from disk.
2. Inject active `HEARTBEAT.md` plus runtime affordances.
3. Run a bounded tool loop.
4. Extract an optional user-facing `<sendMessage>`.
5. Schedule the next wake or fall back to the configured interval.

Heartbeat does not force recap files or daily memory notes. Durable notes happen
only when the character uses write-capable tools. Dormancy stops autonomous LLM
calls until user engagement resumes. Cache keepalive is separate from heartbeat;
it preserves Anthropic cache warmth and does not simulate autonomy.

## Provider Boundary

`shore-llm` owns provider-specific request construction, streaming, response
parsing, retry behavior, content block handling, thinking/reasoning block
translation, and cache breakpoint placement.

Provider wire behavior should be verified with recorded or live provider
responses before release when request formatting, streaming, tool use, thinking,
or cache economics are in scope. Live checks may cost money.

## Clients And Bridges

`shore-matrix` bridges Matrix rooms to SWP messages. Embedded mode manages a
conduwuit-compatible homeserver; external mode connects to an existing Matrix
server. Matrix is a client bridge, not a trusted state store.

`shore-mcp` is for development and agent-driven verification. It speaks to the
daemon through the same SWP path as other clients. Its default profile is
isolated; main-profile writes require explicit writable attachment.

## Observability

Useful commands:

```sh
RUST_LOG=shore_daemon=debug,shore_llm=debug,shore_swp_server=debug shore-daemon
RUST_LOG=shore_cli=debug shore status
shore status
shore status --diagnostics
shore usage
shore usage --anomalies
shore log --heartbeat
shore memory dreams
```

Persistent surfaces:

| Surface | Location |
| --- | --- |
| Usage ledger | `$XDG_DATA_HOME/shore/ledger.db` |
| Cache forensics | `$XDG_DATA_HOME/shore/cache_forensics.jsonl` |
| Conversation log | `$XDG_DATA_HOME/shore/<Character>/active.jsonl` |
| Compacted segments | `$XDG_DATA_HOME/shore/<Character>/segments/` |
| Active prompt snapshot | `$XDG_DATA_HOME/shore/<Character>/active_prompt/` |
| Deferred prompt edits | `$XDG_DATA_HOME/shore/<Character>/deferred_edits.jsonl` |
| Dreaming state | `$XDG_DATA_HOME/shore/<Character>/dreams/` |
| TUI log | `$XDG_DATA_HOME/shore/tui.log` |

Enable cache forensics with:

```toml
[advanced]
cache_forensics = true
```

Provider request bodies can include sensitive conversation context; do not paste
private logs into docs or commits.

## Validation

Use the narrowest useful check first:

```sh
python3 scripts/harness-check.py
cargo fmt --all --check
cargo test -p shore-daemon engine::prompt
cargo test -p shore-daemon tools::workspace
cargo test -p shore-daemon memory::deferred_edits
cargo test -p shore-daemon --test suite
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Release build gate:

```sh
cargo build --release -p shore-daemon -p shore-cli -p shore-tui -p shore-matrix
```

Before a release, also run relevant cache tests, live provider smoke tests if
provider behavior changed, and Matrix live verification if Matrix behavior
changed.

## Removed Runtime Architecture

These are no longer normal runtime architecture:

- authoritative SQLite memory entries table
- LanceDB/vector store as memory source of truth
- passive RAG prompt injection
- separate collation pipeline
- interactive memory shell
- `character.md` as the active character definition path
- compaction-generated recap prompt files
- `memories/` as a runtime memory directory

SQLite is still used for the usage ledger and may appear in migration
tooling/history.
