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
| `dev/test-harness` | `shore-test-harness` | integration harness and mock server |

Out-of-tree clients live in separate repositories and consume the core
library crates (`shore-protocol`, `shore-config`, `shore-swp-client`,
`shore-diagnostics`) from crates.io:

| Crate | Repo |
| --- | --- |
| `shore-tui` (terminal UI) | [mythofmeat/shore-tui](https://github.com/mythofmeat/shore-tui) |
| `shore-gui` (Tauri desktop) | [mythofmeat/shore-gui](https://github.com/mythofmeat/shore-gui) |
| `shore-gui-godot` (Godot client) | [mythofmeat/shore-gui-godot](https://github.com/mythofmeat/shore-gui-godot) |
| `shore-matrix` (Matrix bridge) | [mythofmeat/shore-matrix](https://github.com/mythofmeat/shore-matrix) |
| `shore-mcp` (debug/development MCP) | [mythofmeat/shore-mcp](https://github.com/mythofmeat/shore-mcp) |

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
`hello` character metadata carries optional base64 avatar data, and
`new_message` events carry the authoritative character name and message origin.
Passive clients such as desktop notifiers can therefore label and icon messages
without reading the daemon's local config filesystem.

Handshake and push `History` snapshots contain the active `active.jsonl` tail
only, keeping passive clients and bridges fast. Bounded log/history command
responses may include compacted `segments/` before that active tail; the SWP
`active_start` index marks the first message that remains in prompt context so
clients can draw an archive boundary without treating old scrollback as active
model context.

## File Layout

Config:

```text
$XDG_CONFIG_HOME/shore/
  config.toml
  .env
  characters/<Character>/avatar.png
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
  images/
    attachments/
    generated/
```

Global data:

```text
$XDG_DATA_HOME/shore/ledger.db
```

Cache:

```text
$XDG_CACHE_HOME/shore/cache_forensics.jsonl
$XDG_CACHE_HOME/shore/providers/<Provider>/models.json
$XDG_CACHE_HOME/shore/characters/<Character>/workspace_index.json
$XDG_CACHE_HOME/shore/resized/
$XDG_CACHE_HOME/shore/debug/api_logs/
$XDG_CACHE_HOME/shore/debug/api_logs_long/
```

Per-call API payload logs split into two retention tiers. `api_logs/` is
high-volume per-turn chat traffic — useful for a few days after a bug
shows up. `api_logs_long/` holds background-task payloads (compaction,
dreaming, heartbeat) flagged with `LlmRequest::retain_long`; those calls
are low-frequency but high-value for forensic analysis of cache
regressions and memory drift, so operators typically keep them longer.
Pruning is operator-managed (no internal rotation):

```sh
find ~/.cache/shore/debug/api_logs/      -type f -mtime +3  -delete
find ~/.cache/shore/debug/api_logs_long/ -type f -mtime +30 -delete
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
notifications, and startup diagnostics require restart.

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
writes, tool loop bookkeeping, activity tracking, image cache warmups, and
compaction of the recent conversation tail when the pinned system prompt prefix
is unchanged. Expected cache breakpoints include activating staged prompt edits
at compaction/reload, editing old conversation messages, changing
model/provider/cache settings, and changing prompt templates or tool
definitions in code.

Cache invalidation accounting is split into two layers:

| Layer | Expected invalidation paths | Must stay cache-stable |
| --- | --- | --- |
| Provider-side Anthropic prompt cache | Cache TTL expiry without a successful keepalive, model/provider/cache setting changes, thinking-mode shape changes, prompt template or tool definition changes, activation of staged prompt-visible edits, edits to already-cached conversation history, and explicit cache breakpoint/debug overrides. | Ordinary workspace writes, markdown memory writes before active-prompt activation, tool-loop bookkeeping, activity stats, image cache warmups, and compaction of only the recent conversation tail when the pinned system prefix is unchanged. |
| Shore cached `last_request` reuse | Successful chat requests replace it; successful compaction clears it because the conversation tail changed. Heartbeat and keepalive may rebuild it from disk. | Clearing `last_request` must not clear the cache keepalive deadline by itself; the pinned provider-side system prefix may still be worth refreshing. |

Local regression tests should cover request-shape invariants before live
provider checks are run. In particular, Anthropic cache-control placement tests
must account for generated chat histories, tool-loop tails, system anchors, the
four-breakpoint provider limit, and the rule that the active final user message
is never itself a message breakpoint. Live cache scripts under
`scripts/cache-tests/` validate provider behavior and economics only after
those local invariants pass.

An observed cache-read decrease while the ledger believes the cache is warm is
not an expected invalidation path. It is recorded as `UnexpectedWrite` and must
be treated as a regression signal unless explained by a known deliberate
breakpoint above. Tool-loop calls keep a separate short-lived cache-read
baseline because their request prefix advances through newly completed
`tool_result` blocks; within a loop that baseline must not drop, and the first
tool-loop continuation after a warm message must not rewrite the prefix with
zero cache read. Request-shape tests should keep tools, system blocks, and
already-existing messages byte-preserved for every generation variant; only
configured tool-surface changes or explicit/manual history edits may change
that prefix.

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
Archived segments stay available to client history/log views through bounded,
lazy pages, but prompt assembly and normal history snapshots use only the
retained active tail.

Compaction is single-flight per character. Manual `shore memory compact` and
idle-triggered compaction share the same guard, so a second pass returns `busy`
instead of racing against the same active transcript and memory files.

Dreaming is an opt-in scheduled AI librarian pass. When autonomy and
`[memory.dreaming]` are enabled, the character privately uses memory/workspace
tools to inspect, dedupe, consolidate, and mark stale or superseded memory.
The schedule is a five-field cron expression. Dreaming may edit prompt-visible
files; those edits follow the same deferred activation rule.

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
stored at `$XDG_CACHE_HOME/shore/characters/<Character>/workspace_index.json`;
markdown files remain authoritative and the index can be deleted and rebuilt.
Search/index walks the whole workspace tree (including `memory/`) with
configurable file-size, file count, and total-byte limits.

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
Remote clients do not discover daemons through the local instances registry;
they should use `SHORE_ADDR`, `--addr`, or
`$XDG_CONFIG_HOME/shore/client.toml`.

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
it preserves Anthropic cache warmth and does not simulate autonomy. Compaction
clears any cached request body that contains the old conversation tail, but it
does not cancel the keepalive deadline; a later keepalive can rebuild from disk
to keep stable pinned system prompt sections warm.

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
SHORE_MATRIX_RUST_LOG=shore_matrix=debug shore-daemon
RUST_LOG=shore_cli=debug shore status
shore status
shore status --diagnostics
shore usage
shore usage --budget
shore usage --by-kind
shore usage --by-api-key
shore usage --anomalies
shore log --heartbeat
shore memory dreams
```

The usage ledger records provider/model, raw `call_type`, finish reason,
configured API key name, token counts, cache TTL, cache state/anomalies, and
cost components plus cost provenance. When OpenRouter includes `usage.cost`,
Shore stores that provider-reported billed total and marks the row
`provider_reported`; catalog pricing is still used as a fallback estimate when
the provider does not report a total. `shore usage --by-kind` rolls raw rows
into user-facing categories such as `message_no_tools`, `message_with_tools`,
`heartbeat`, `compaction`, and `dreaming`; `shore usage --by-api-key` groups
spend by the friendly configured key name, with historical rows shown as
`unknown`.

Usage budgets are configured under `[usage]` and evaluated directly against the
ledger before each LLM call. Budget windows use the configured calendar
timezone (`local` by default), can scope by provider/model/API key/character or
usage kind, and can warn, block, or pause background work. Compaction is allowed
over blocking budgets by default so context reduction can still lower future
spend; operators can disable that globally or per budget. `shore usage
--budget` reports current budget state and optional spike warnings, and
`--json` exposes the same data for clients. When a completed call crosses a
budget warning threshold, the daemon records that budget/window/threshold and
emits a request-scoped `usage_warning` frame plus the matching notification
event, so CLI/TUI clients can surface it without polling `shore usage`.

Long-running daemon service logs default to a scoped filter:
`warn,shore_daemon=info,shore_llm=info,shore_ledger=info,shore_swp_server=info`.
The daemon-supervised Matrix bridge gets its own `RUST_LOG` from
`SHORE_MATRIX_RUST_LOG`, defaulting to
`warn,shore_matrix=info,matrix_sdk_crypto::backups=error`, so routine Matrix
SDK sync and key-backup chatter does not dominate the daemon's systemd journal.
Service logs use a single-line human format with the event sentence first,
followed by structured event fields and span context:
`LEVEL target: message | fields: key=value ... | spans: span{field=value}`.

Persistent surfaces:

| Surface | Location |
| --- | --- |
| Usage ledger | `$XDG_DATA_HOME/shore/ledger.db` |
| Conversation log | `$XDG_DATA_HOME/shore/<Character>/active.jsonl` |
| Compacted segments | `$XDG_DATA_HOME/shore/<Character>/segments/` |
| Active prompt snapshot | `$XDG_DATA_HOME/shore/<Character>/active_prompt/` |
| Deferred prompt edits | `$XDG_DATA_HOME/shore/<Character>/deferred_edits.jsonl` |
| Dreaming state | `$XDG_DATA_HOME/shore/<Character>/dreams/` |
| Image attachments and generated outputs | `$XDG_DATA_HOME/shore/<Character>/images/` |

Disposable cache surfaces:

| Surface | Location |
| --- | --- |
| Cache forensics | `$XDG_CACHE_HOME/shore/cache_forensics.jsonl` |
| Provider model discovery | `$XDG_CACHE_HOME/shore/providers/<Provider>/models.json` |
| Workspace embedding index | `$XDG_CACHE_HOME/shore/characters/<Character>/workspace_index.json` |
| Resized image cache | `$XDG_CACHE_HOME/shore/resized/` |
| API payload debug logs (chat) | `$XDG_CACHE_HOME/shore/debug/api_logs/` |
| API payload debug logs (background, long-retention) | `$XDG_CACHE_HOME/shore/debug/api_logs_long/` |

Runtime surfaces:

| Surface | Location |
| --- | --- |
| TUI log | `$XDG_RUNTIME_DIR/shore/tui.log` |

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

CI also runs a visibility-only coverage report:

```sh
cargo llvm-cov --workspace --all-targets --lcov --output-path lcov.info
```

Release build gate:

```sh
cargo build --release -p shore-daemon -p shore-cli
```

The workspace correctness ratchet is intentionally compiler-enforced. Clippy
pedantic runs workspace-wide, cleaned crates deny panic-hygiene and lossy-cast
lints at the crate root, and Tier 2 also denies bare `#[allow]` suppressions,
panics/unwraps inside `Result` functions, `let _ =` discards of must-use values,
ignored return values, and unchecked `as` conversions. The low-noise Tier 2 set
also locks ref-counted pointer clone style, single-variant wildcard matches,
`dbg!`, stdout/stderr print macros, `std::process::exit`, `mem::forget`,
undocumented unsafe blocks, one unsafe op per `unsafe {}` block, assert
messages, `unsafe_code`, elided lifetimes in paths, unused qualifications,
missing `Debug` implementations, and unreachable `pub` items.
Import and literal hygiene is locked too: no wildcard imports (`use foo::*`),
separated numeric-literal suffixes (`1_u64`, not `1u64`), and descriptive
(non-single-char) lifetime names. String discipline is locked too:
`string_slice` bans `&s[i..j]` (a panic class on non-char-boundary byte
indices) and `str_to_string` prefers `.to_owned()` over `.to_string()` on
`&str`. Arithmetic discipline is locked too: `integer_division` forces
truncating `/` to be acknowledged, `modulo_arithmetic` flags `%` sign-surprises,
and `float_arithmetic` flags precision/NaN-prone float math (float-heavy
functions carry a reasoned function-level `#[expect]`). Suppressions must use
`#[expect(..., reason = "...")]`.

Before a release, also run relevant cache tests, live provider smoke tests if
provider behavior changed, and Matrix live verification if Matrix behavior
changed.

## Correctness Ratchet

### Rationale

The ratchet exists so that heavy AI-assisted development cannot silently
degrade the codebase. It is a CI ratchet: quality-gating checks that block
merges and can only hold or improve over time, never regress. The long-term
aim is to make it as close to *impossible for bad code to compile* as the lint
surface allows — minimal exceptions, minimal escape hatches. Every gate is a
pure internal-hardening change, invisible to end users, leaving the daemon
functional after every merge.

### Rollout Convention

New lints land through a fixed sequence so the baseline only tightens:

- **Spike one crate first.** Enable the lint on a single crate to get a real
  violation count before committing to a workspace-wide cleanup.
- **Stage `warn` → per-crate lock → workspace promotion.** Start as a workspace
  `warn`, then add `#![deny(...)]` at the crate root once a crate is clean, then
  promote to `[workspace.lints]` so new crates inherit the baseline
  automatically. CI's `-D warnings` keeps the promoted set hard.
- **No bare suppressions.** There are `0` bare `#[allow]` in the tree. Every
  suppression is a reasoned `#[expect(..., reason = "...")]`, so a suppression
  that stops being needed fails the build instead of lingering.

### Tier Model

- **Tier 1** — `clippy::pedantic` workspace-wide, panic-hygiene and lossy-cast
  lints deny-locked per crate, `deny.toml` dependency hygiene, and
  `#[expect(..., reason = …)]` discipline.
- **Tier 2** — draconian `clippy::restriction` plus rustc paranoia: no panics
  or unwraps inside `Result` functions, no `let _ =` discards of must-use
  values, no ignored return values, no unchecked `as` conversions, locked
  ref-counted clone style, banned `dbg!`/print macros/`process::exit`/
  `mem::forget`, documented unsafe blocks, no unreachable `pub` items, and no
  variable shadowing (a binding can never silently shadow another, so data flow
  stays explicit).
- **Tier 2/3 tests** — `insta` snapshots, `proptest` round-trips, and a
  `cargo-llvm-cov` coverage job for visibility.

The Validation section above lists the currently enforced set; this section is
the convention every new gate follows.

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
