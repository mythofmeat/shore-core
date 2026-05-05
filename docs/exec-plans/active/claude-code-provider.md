# Claude Code as LLM Provider

Status: implemented; live CLI smoke, MCP tool-call, and multi-turn daemon
soak verified 2026-05-05
Owner: agent
Started: 2026-05-04

## Goal

Add a new LLM provider that drives the local `claude` CLI subprocess
in non-interactive mode, billing against the user's Claude
subscription (Pro/Max/Team) instead of per-token API charges. shore
keeps owning conversation state, character workspace, autonomy/
heartbeat/dreaming loops, and tool execution. The CLI's only job is
to be the OAuth-authenticated bridge to Anthropic's models.

## Context

- Phase-0 spike findings: [`dev/spikes/claude-code-probe/FINDINGS.md`](../../../dev/spikes/claude-code-probe/FINDINGS.md)
- shore-llm provider trait pattern: free functions per provider in
  `backend/llm/src/providers/{anthropic,openai,gemini,zai}.rs`,
  dispatched via `Sdk` match in `backend/llm/src/providers/mod.rs`.
- Tool dispatch entry point: `backend/daemon/src/tools/mod.rs:318`
  (`dispatch_tool`).
- Tool loop runner: `backend/daemon/src/engine/tools.rs:48`
  (`run_tool_loop`) — naturally a no-op for claude-code requests
  because the CLI runs its own internal loop and emits no
  `tool_use` blocks back to shore-llm.

## Architecture

```
┌────────────┐      LlmRequest      ┌────────────┐
│ shore-     │ ───── HTTP/JSON ───▶ │ shore-llm  │
│ daemon     │                      │ (provider  │
│ (engine,   │ ◀── StreamEvent ──── │  match)    │
│ tools/*)   │     NDJSON stream    │            │
│            │                      └─────┬──────┘
│ ┌────────┐ │                            │ spawns
│ │ MCP    │ │                            ▼
│ │ session│ │                      ┌────────────┐
│ │ HTTP   │ │ ◀──── tools/call ──  │ claude -p  │
│ │ srv    │ │ ────  result ───────▶│ (subprocess)│
│ └────────┘ │                      └────────────┘
└────────────┘
```

The daemon hosts a per-session MCP HTTP endpoint. shore-llm spawns
the CLI with `--mcp-config` pointing at that endpoint. Tool calls
flow CLI → daemon directly (not through shore-llm), so shore-llm
stays thin and stateless. The session's MCP listener has the
character + workspace context baked in for the lifetime of one
chat request.

### Why daemon-hosted MCP, not shore-llm-hosted

shore-llm has no tool handlers — those live in
`backend/daemon/src/tools/`. Routing tool calls through shore-llm
would mean either duplicating the dispatch path or threading
mpsc/HTTP back-channels for every call. Daemon-hosted MCP reuses
`dispatch_tool` directly with no plumbing.

### Pattern 3 hybrid from day one

Long-lived `claude -p` subprocess per active conversation, with
fresh-spawn fallback for cold starts and post-compaction restarts.
The live subprocess preserves user/assistant turn pairs and
in-process `thinking` blocks across consecutive turns; the
fresh-spawn path renders prior conversation as a transcript in
the system prompt when no live subprocess exists yet (or a prior
one died, or the character was just compacted/dreamed). That
fresh-spawn transcript is intentionally good-enough recovery, not
equivalent to Claude Code's native structured turn history: tool
records are visible as tagged text until the long-lived subprocess
has rebuilt live context.

Lifecycle (engine-owned, see Phase C):

1. First chat request for a character: spawn subprocess with
   transcript-in-system-prompt covering all prior persisted turns.
   Cache the subprocess handle keyed by `(character, conversation)`.
2. Subsequent chat requests for the same conversation: write a
   single `{"type":"user",...}` frame to the live subprocess's
   stdin; read until the next `result` event.
3. On compaction, dreaming, character switch, daemon restart, or
   subprocess death: tear down the live subprocess. The next
   request re-bootstraps from persisted history via fresh-spawn.
4. Idle timeout: tear down after N minutes of inactivity (config-
   gated; tentative default 15 minutes) to free resources.

### What's deliberately dropped

- **Streaming to clients.** shore-llm still returns a `StreamEvent`
  NDJSON stream, but events are emitted only as the CLI's
  stream-json output is parsed; client SSE streaming through to
  TUI/GUI is not preserved.
- **Anthropic prompt caching.** Irrelevant under flat-fee.
- **Cross-turn thinking.** See above.

## Open Questions To Resolve Before Coding

These need quick verification probes, not full implementation work.

- [x] Does `--mcp-config` accept HTTP-transport configs as JSON
  strings, or only stdio? If HTTP works, daemon's new HTTP listener
  hosts the per-session MCP endpoint and shore-llm passes a URL.
  If not, we ship a tiny `shore-mcp-bridge` stdio binary. (Probe 8.)
- [x] Confirm the CLI tolerates a 10-100KB system prompt. shore's
  active prompt + transcript can be sizeable for fresh-spawn
  bootstraps. (Probe 9.)
- [x] Daemon currently runs SWP only (no HTTP) — confirmed. New
  HTTP listener is in scope; mount it on a config-controlled bind
  address parallel to the SWP listener. May need a new
  `[daemon.http]` block.

## Work Items

### Phase A: research & scaffolding

- [x] **Probe 8**: HTTP-transport `--mcp-config` viability.
- [x] **Probe 9**: large system-prompt tolerance.
- [x] Add `Sdk::ClaudeCode` variant in `core/config/src/models.rs`,
      with config schema (`sdk = "claude_code"`).
- [x] Decide MCP transport (stdio bridge vs HTTP) based on Probe 8.

### Phase B: shore-llm provider

- [x] New `backend/llm/src/providers/claude_code/`:
  - [x] Stream-json parser: handle `system`, `assistant`, `user`,
        `result`, `rate_limit_event` events. Map assistant blocks
        (`text`, `thinking`, `tool_use`) to `StreamEvent`s.
        Tolerate multiple `system init` events per subprocess
        lifetime (one per turn under pattern 3 long-lived mode).
  - [x] Per-request MCP config: read `provider_options.mcp_endpoint`
        and `provider_options.allowed_tools` from the request,
        build the right CLI flags.
  - [x] Subprocess driver, **fresh-spawn path** (cold starts and
        post-compaction): spawn CLI per request; render prior
        history as transcript in `--system-prompt`; write one
        user frame; read to completion; close stdin; return.
  - [x] Subprocess driver, **long-lived path**: keyed handle
        cache by `(character, conversation)` (cache lives in
        shore-llm process; key passed via
        `provider_options.subprocess_key`). On cache hit, write
        one user frame to the live subprocess; on miss, fall back
        to fresh-spawn and populate the cache. Idle eviction
        timer.
  - [x] `generate(client, request)` and `stream(client, request)`:
        both route through the same subprocess driver. `generate`
        collects events into a `GenerateResponse`; `stream` emits
        them as `StreamEvent` NDJSON as parsed.
  - [x] Quota error handling: parse `out of extra usage` /
        `rate_limit_event` and surface as a typed `LlmError`
        variant. Quota errors should evict the subprocess from
        the cache.
- [x] Register the provider in `providers/mod.rs` `match` arms.
- [x] Unit tests for the parser using captured spike fixtures
      (`dev/spikes/claude-code-probe/results/*.jsonl`).
- [x] **Live test**: stand up shore-llm against a real `claude`
      install with the daemon's MCP listener mocked via the spike
      fixture; assert end-to-end stream-json → StreamEvent NDJSON
      shape on a fresh-spawn turn and a long-lived follow-up turn.

### Phase C: daemon-side MCP host

- [x] New `[daemon.http]` config block (bind address, off by
      default unless any `claude_code` chat model is configured).
- [x] HTTP listener task in the daemon, parallel to the SWP
      listener. Uses `axum` (already a likely candidate; verify
      against current deps). Routes initially:
      `POST /mcp/<session-token>` for the MCP transport.
- [x] New module `backend/daemon/src/engine/mcp_session.rs`:
  - [x] Per-request MCP session tied to character + workspace
        context. Identified by an opaque session token in the URL.
  - [x] Speak MCP `initialize`, `tools/list`, `tools/call`.
  - [x] `tools/list` enumerates tools from active
        `behavior.tool_use.tools` config + character workspace
        scope.
  - [x] `tools/call` dispatches via `tools::dispatch_tool` with
        the bound character workspace + conversation context.
        Translate `ContentBlock` results into MCP `content` array.
  - [x] **Per-session tool-call ledger**: every served call
        records `(tool_use_id, name, input, content_blocks,
        is_error)` into a buffer the engine reads after the chat
        request completes. See "Tool-call observability" below.
  - [x] Transport: HTTP-streamable if Probe 8 passes; otherwise
        Unix socket consumed by a stdio bridge binary.
  - [x] Session lifetime tied to the chat request that created it
        for fresh-spawn pattern; tied to the cached subprocess
        for long-lived pattern.
- [x] Wire it into `handler/task.rs`:
  - [x] When the resolved model has `sdk = ClaudeCode`, allocate
        a session MCP listener before calling shore-llm; tear it
        down after.
  - [x] Populate `LlmRequest.provider_options.mcp_endpoint` and
        `provider_options.allowed_tools` (built from the active
        tool registry).
- [x] **Tool-call observability for conversation history.** With
      claude-code, the model's tool calls happen inside the CLI
      via MCP; shore-llm only sees the final assistant message.
      Without explicit handling, shore's conversation log loses
      the intermediate tool_use/tool_result blocks that compaction
      and dreaming rely on. The MCP listener must record every
      `(tool_use_id, name, input, result)` it serves and the
      daemon must splice these as synthetic content blocks into
      the conversation history before persisting the assistant
      turn. Without this, characters lose the ability to refer
      back to "the file I just read" / "what I searched for" in
      future turns.
- [x] If stdio bridge needed: new bin in `backend/mcp-tool-bridge/`.
      Not needed; HTTP transport passed.

### Phase D: config + UX

- [x] Config:
  ```toml
  [chat.claude_code.opus-max]
  model_id = "claude-opus-4-5"
  ```
  (The provider key `claude_code` and `sdk = "claude_code"` is
  inferred from the namespace; no `api_key_env` since OAuth lives
  in `~/.claude/`.)
- [x] Update [CONFIGURATION.md](../../../CONFIGURATION.md) with the
      new provider section.
- [x] Update [README.md](../../../README.md) under feature overview
      (the repo no longer has `FEATURES.md`).
- [x] Update [ARCHITECTURE.md](../../../ARCHITECTURE.md) noting the
      "shore-llm-spawns-CLI" sidecar shape.
- [x] Doctor / `shore config --check`:
  - [x] Detect missing `claude` binary on PATH.
  - [x] Detect logged-out auth state (best-effort: parse
        `claude auth status --json`).

### Phase E: telemetry + quota

- [x] Surface `total_cost_usd` and `modelUsage` from the CLI's
      `result` event. Tag as "would-be-API cost" not "actual
      spend" in display.
- [x] Replace cache-health pane with a "Max subscription" badge
      for this provider in CLI usage output.
- [x] Surface `rate_limit_event` info in `shore usage`.
- [ ] Optional fallback: when ClaudeCode quota is exhausted, fall
      back to `[chat.anthropic.<alias>]` if a peer entry exists.
      Off by default (could surprise-cost the user).

### Phase F: validation

Continuous validation requirement: after every Phase B/C work-item
completes, exercise it against an actual daemon (the test profile
under `$XDG_DATA_HOME/shore-mcp-test/` per `dev/mcp/README.md`)
with a real `claude` binary. Don't rely on unit tests alone — the
Phase 0 spike showed at least three places where empirical
behavior contradicted the documented behavior (assistant frames,
permission gate, --bare/OAuth). Treat the live daemon as part of
the test harness, not a final-step integration check.

- [x] Deterministic harness probe under `dev/test-harness/` that:
  - [x] Exercises the daemon MCP listener with unit/integration tests.
  - [x] Exercises the subprocess cache with fake-CLI fixture scripts that emit fixed
        stream-json transcript including a `tools/call` against
        the mocked MCP.
  - [x] Exercises the parser, MCP roundtrip, and `GenerateResponse`
        construction.
- [x] Live integration test, end-to-end with a real `claude`
      binary and the test daemon profile:
- [x] Fresh-spawn turn against the spike HTTP MCP server calls the real
        `ping` tool through the CLI; `tools/call` is verified in the
        MCP log.
  - [x] Long-lived subprocess cache behavior covered by fake-CLI
        cache-hit, mismatch, and dead-child tests.
  - [x] Full multi-turn daemon profile with memory write/read and
        compaction-triggered teardown verified manually on 2026-05-05
        against release binaries.
- [x] `cargo fmt --all --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`, `cargo test --workspace`.
- [x] `python3 scripts/harness-check.py` (per AGENTS.md).

## Validation

- Spike fixtures: `dev/spikes/claude-code-probe/results/*.jsonl`
  → unit-test inputs for the parser.
- Daemon harness: `cargo test -p shore-daemon engine::mcp_session`.
- `python3 scripts/harness-check.py` (per AGENTS.md).
- Live release-binary `shore` daemon soak against a throwaway character:
  Claude Code wrote a durable `memory/...` file through Shore MCP, read
  it back, automatic max-turn compaction completed and reloaded the
  engine after the CLI disconnected, and the next Claude Code turn read
  the same memory file after the compaction boundary.

## Decisions

- 2026-05-04: **Pattern 3 hybrid from day one.** Long-lived
  subprocess per active conversation for faithful user/assistant
  turn-pair preservation including in-process `thinking` blocks;
  fresh-spawn fallback for cold starts and post-compaction.
  Reason: user wants turn pairs faithfully used; pattern 1 alone
  loses too much fidelity to be acceptable. Cost: more lifecycle
  complexity in the daemon's engine module.
- 2026-05-04: **Daemon hosts MCP, not shore-llm.** Reuses
  `dispatch_tool` directly; keeps shore-llm thin; eliminates
  cross-process tool-result back-channels.
- 2026-05-04: **Daemon grows an HTTP listener.** Required for
  HTTP-transport MCP if Probe 8 passes; useful beyond this plan
  (anticipated future routes for diagnostics, tool surfaces, etc.).
  Bind address config-controlled, default-off unless a claude_code
  model is configured.
- 2026-05-04: **Tool-call splicing into conversation history.**
  The daemon's MCP listener buffers every served `tools/call` and
  the engine merges them as synthetic `tool_use` + `tool_result`
  ContentBlocks into the persisted assistant turn. Without this,
  compaction and dreaming lose visibility into the character's
  in-turn actions.
- 2026-05-04: **shore owns conversation state.** The CLI is a
  transport. Long-lived subprocesses cache the state-as-context
  but shore re-bootstraps from persisted history on any boundary
  (compaction, dreaming, restart, idle eviction).
- 2026-05-04: **Client-streaming dropped.** SSE-to-TUI/GUI through
  this provider is not preserved; events flow internally.

## Risks

- **CLI version drift.** Flag surface and stream-json shape can
  change between Claude Code releases. Mitigation: pin a minimum
  version string, verify with `claude --version` at startup, log
  warning on drift, keep parser tolerant to unknown event types.
- **OAuth interruption.** If `~/.claude/` credentials expire
  mid-session, calls fail. Mitigation: surface auth-error message
  cleanly from the result event, suggest `claude auth login`.
- **2KB Claude Code overhead per call.** Burns into 5h rate
  window. Mitigation: documented in FINDINGS.md; not blocking
  under flat fee but inflates "savings vs API" telemetry by a
  hair. Display API cost reflects what the call really cost the
  CLI, not what shore would have spent direct.
- **MCP transport choice.** If HTTP `--mcp-config` doesn't work,
  we ship a small bridge binary — not blocking, but adds an artifact
  to the build/install story.
- **Concurrency.** Multiple characters in autonomy mode could
  spawn parallel CLI subprocesses, hitting the 5h shared rate
  window. Mitigation: serialize daemon-side or surface a global
  rate-limit observer for the user. Out of scope for v0.

## Handoff Notes

For a continuing agent: the Phase-0 spike is at
`dev/spikes/claude-code-probe/`. `FINDINGS.md` is the architectural
truth source. The two probes still to run before any code is
written are listed under "Open Questions". After those, Phase A
gates on enum changes and config wiring; Phase B (provider) and
Phase C (daemon MCP host) can proceed in parallel once the wire
contract for `provider_options.mcp_endpoint` is settled.

The existing chat providers have very different shapes — pick
`anthropic.rs` as the closest analog for request shape and
StreamEvent emission, but expect the subprocess-driver code path
to look more like `backend/daemon/src/engine/...` than like the
HTTP-client providers.
