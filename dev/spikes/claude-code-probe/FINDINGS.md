# Claude Code as shore LLM Backend — Phase 0 Findings

Spike target: `claude` 2.1.126.
Auth: OAuth via claude.ai (Pro subscription on test account; identical
auth flow on Max).

## Verdict

**Architecture is viable.** The four hard-yes/no questions came back
yes. shore can drive `claude -p` as a clean Anthropic frontend, expose
shore's character-workspace tools via MCP, keep the daemon owning
the loop and conversation state, and bill against the user's Max
subscription.

There are real constraints. They are listed at the bottom.

## What works

### 1. Full system-prompt override

`--system-prompt <prompt>` (distinct from `--append-system-prompt`)
fully replaces Claude Code's default agent prompt. Probe 1 used a
"librarian-haiku" system prompt; the model returned a haiku and
nothing else — no coding-agent leakage.

### 2. Built-in tools fully disabled

`--tools ""` zeroes every built-in (Bash, Read, Edit, Write, Task,
…). The system-init event lists `tools: []` for built-ins.
**However** user-installed MCP tools from `~/.claude/` are still
present unless `--strict-mcp-config` is also passed. Both flags are
required for full isolation.

### 3. MCP tool round-trip

Stand up a stdio MCP server, register it via `--mcp-config`, and the
model emits `tool_use` blocks → MCP server runs → `tool_result`
blocks come back → model continues. Round-trip is clean and the
MCP server log lines up with the stream-json events.

**Permission gate is mandatory.** Without `--allowedTools`, every
MCP tool call is intercepted by Claude Code's permission system with:

> "Claude requested permissions to use mcp\_\_shore-spike\_\_ping, but
> you haven't granted it yet."

The MCP server never sees `tools/call` in that case. shore's provider
must build `--allowedTools "mcp__<server>__<tool>,…"` from its own
tool registry on every invocation.

Tool naming: `mcp__<server-name>__<tool-name>` where `<server-name>`
is the key under `mcpServers` in the MCP config.

### 4. Stream-json input/output schema

Top-level event types observed:

- `system` (`subtype: init`) — once at start; describes available
  tools, MCP servers, model, session id, memory paths, slash
  commands, plugins, agents.
- `rate_limit_event` — current quota state with `status`, `resetsAt`,
  `rateLimitType: "five_hour"`, `overageStatus`,
  `overageDisabledReason`.
- `assistant` — model output, `message.content[]` of blocks.
- `user` — emitted when MCP returns a `tool_result` block.
- `result` — final summary with `is_error`, `result` text,
  `total_cost_usd`, `usage`, `modelUsage` (per-model breakdown),
  `num_turns`, `stop_reason`, `terminal_reason`.

Assistant block types:

- `text` — plain text
- `tool_use` — `{type, id, name, input}`
- `thinking` — `{type, thinking, signature}` (signature ~2KB)
- `redacted_thinking` — expected, not observed

User block types in tool-result context:

- `tool_result` — `{type, tool_use_id, is_error, content}`

Cost reporting in the `result` event still surfaces `total_cost_usd`
and per-model `modelUsage` even on OAuth — gives shore a "would
have cost on API" number for the savings-vs-API badge without
tracking it ourselves.

## What does not work, and the workaround

### A. stream-json input is a live user stream, not a transcript

`--input-format stream-json` models a stream of user-typed inputs
to a live agent. Multiple user frames stitch into one conversation
(verified: "remember PURPLE" → "what did I ask?" works across two
frames in one invocation). Assistant frames in stdin are silently
discarded — they are not part of the input schema. The CLI is the
agent; it generates assistant turns itself.

Confirmed by feeding `assistant: "Is the word BARNACLE-ZIRCON-9947?"`
between two user frames: the model later answered "I DIDN'T GUESS
ONE", proving it never saw the supplied text. Behavior was identical
with that frame removed.

**Implications for shore — three viable patterns:**

1. **Fresh subprocess per turn, transcript in system prompt or
   first user frame.** shore retains full ownership of state. Cross-
   turn `thinking` blocks are unrecoverable (they're signed and
   can't be replayed as text); cross-turn text is preserved by
   serialization. Compaction works naturally. Heartbeat/dreaming
   work naturally.

2. **Long-lived subprocess per conversation.** Keep one `claude -p`
   alive; feed each new user message as a stream-json frame; read
   to the next `result` event; loop. CLI maintains internal
   history including in-turn `thinking` blocks across what shore
   would call multiple turns. Costs: persistent process per active
   conversation, history lost on subprocess death, cannot compact
   or strip turns mid-conversation.

   **Verified working** (probe 07): subprocess stays alive between
   frames, context threads across turns ("remember PURPLE" → "what
   was the word?" → "PURPLE" → "reverse that" → "ELPRUP"), clean
   exit (rc=0) on stdin close. Quirk: the CLI re-emits a `system`
   init event before each turn's response (same `session_id`
   throughout) — shore's stream parser must accept multiple init
   events per subprocess lifetime. The `--print` flag really means
   "exit when stdin closes," not "exit after one response."

3. **Hybrid: long-lived for hot conversations, fresh-spawn for cold
   restarts and post-compaction.** Best of both. Thinking continuity
   during live dialog; full state ownership at compaction/restart
   boundaries.

**Recommendation: implement pattern 1 first**, then add pattern 2
as an opt-in optimization later if cross-turn thinking continuity
matters in practice. Pattern 1 alone is correct and simple; pattern
3 is the eventual destination.

**Note on schema rigidity**: `assistant.message.content` MUST be a
content-block array even when frames would otherwise be discarded.
A bare string content (`content: "text"`) trips a JS-level error
in Claude Code's input parser:
`H.message.content.some is not a function`. shore's provider must
emit the array form even though the frames are then thrown away.

### B. HOME override breaks OAuth

Setting `HOME` to a tempdir results in `loggedIn: false,
authMethod: none`. OAuth credentials live under `~/.claude/`. Per-
character isolation cannot use a per-character HOME without forcing
re-auth.

**Workaround:** isolation comes from `--setting-sources ""` +
`--strict-mcp-config` + `--no-session-persistence` + a unique
`--session-id` per call. OAuth stays in `~/.claude/`; everything
else is per-invocation.

### C. `--bare` disables OAuth

`--bare` is the documented "minimal mode" that strips hooks, plugin
sync, auto-memory, CLAUDE.md auto-discovery, etc. It is the cleanest
isolation surface but it explicitly disables OAuth: "Anthropic auth
is strictly ANTHROPIC\_API\_KEY or apiKeyHelper via --settings (OAuth
and keychain are never read)." Cannot use `--bare` on the Max-
subscription path.

### D. ~2000-token Claude Code overhead per call

Even with `--system-prompt`, `--tools ""`, `--setting-sources ""`,
`--strict-mcp-config`, and `--no-session-persistence`, the input
token count includes ~2KB of cached overhead (606 cache_creation +
1412 cache_read in the test). Source unknown — likely fixed system
sections that survive `--system-prompt` (security instructions,
tool-protocol bootstrap). Only `--bare` strips it, and `--bare`
breaks OAuth.

Implication: doesn't cost real money under Max flat fee, but burns
into the 5-hour rate window. The `total_cost_usd` Claude Code
reports for telemetry will be inflated by this overhead — shore's
"savings vs API" display should reflect the true API cost, which
is a hair higher than what shore-direct-Anthropic would have spent.

## The recipe for shore's `claude_code` provider

```
claude --print
  --output-format stream-json
  --input-format stream-json
  --verbose
  --no-session-persistence
  --setting-sources ""
  --strict-mcp-config
  --mcp-config <character-scoped MCP config>
  --tools ""
  --allowedTools "<comma-separated list of mcp__<server>__<tool>>"
  --model <model-id>
  --system-prompt <SOUL+USER+AGENTS+TOOLS+HEARTBEAT+MEMORY +
                   serialized prior conversation>
  --session-id <fresh UUID per call>
  [--effort low|medium|high|xhigh|max for thinking-capable models]
```

Stdin: one or more `{"type":"user","message":{"role":"user","content":"..."}}`
frames. Stdout: line-delimited JSON event stream as documented above.

## Recommended next step

Write `backend/llm/src/providers/claude_code.rs` implementing
shore's existing chat provider trait. It is a subprocess driver,
not an HTTP client. The work breaks down:

1. **Subprocess management.** Spawn `claude -p` per request, write
   stream-json to stdin, parse stream-json from stdout, surface
   the final `result` event as the response. Kill on
   `behavior.tool_use.max_iterations` if Claude Code's loop runs
   over.
2. **MCP server.** A small in-process MCP stdio server that exposes
   shore's character-workspace tools (`memory_*`, `read`, `write`,
   `edit`, `search`, `list_files`, `exec`, `web_search`, …) by
   delegating to the existing tool dispatch path. This is the bulk
   of new code. The existing tool handlers don't change — they
   already enforce workspace sandboxing — only the surface that
   exposes them changes.
3. **Config wiring.** New `[chat.claude-code.<alias>]` block (or a
   `sdk = "claude-code"` knob on `[providers.<name>]`).
4. **Telemetry.** Surface `total_cost_usd` and `modelUsage` from
   the result event; add a "Max subscription (flat fee)" badge to
   replace the cache-health pane on this provider.
5. **Quota handling.** Parse `rate_limit_event` and the
   "out of extra usage" message shape; surface as a soft error
   that shore can display to the user without tearing down the
   conversation. Optional fallback to `[chat.anthropic.<alias>]`
   if configured.

Streaming is intentionally out of scope. shore can poll the
stream-json output and update progressively if we want a TUI/GUI
spinner, but client-streaming SSE is dropped.

Cross-turn `thinking` is dropped under pattern 1 (fresh subprocess
per turn) and preserved under pattern 2 (long-lived subprocess).
shore should implement pattern 1 first and add pattern 2 later if
in-practice it matters; the existing
`[memory.thinking].preserve_prior_turns` setting already governs
the same trade-off in API-direct mode.

Heartbeat / dreaming work without changes — they're shore-side
loops that issue normal chat requests; the provider just happens
to drive a subprocess.

## Files captured

- `mcp_ping.py` — minimal stdio MCP server used in probe 3.
- `mcp-config.json` — template, rendered into `results/`.
- `probes/0[1-6]-*.sh` — one shell script per probe.
- `results/*.jsonl` — raw stream-json captures (gitignored).
- `results/mcp-ping.log` — MCP server request log (gitignored).
