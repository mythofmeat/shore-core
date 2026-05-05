# Claude Code Provider Parity Notes

Status: current as of 2026-05-05. Verified against the local `claude --help`
surface and the provider implementation in this tree.

The `claude_code` provider is intended to be a drop-in replacement for normal
Anthropic-family chat models for Shore's runtime: chat, memory persistence,
workspace tools, heartbeat, compaction, dreaming, keepalive, usage accounting,
and model selection all route through the same daemon-owned state and tool
paths. The remaining gaps below are places where Claude Code's OAuth-backed CLI
transport does not expose the same controls as the direct Anthropic API or an
OpenRouter Anthropic model.

## Implemented Parity

- Chat requests use the same `LlmRequest`/`StreamResult` contract as other
  providers.
- Shore tools are exposed through the daemon MCP listener and dispatched through
  the existing `dispatch_tool` path.
- Tool calls are recorded in a per-turn ledger and persisted back into history
  as `tool_use`/`tool_result` blocks so compaction and dreaming can see what
  happened.
- Heartbeat, compaction, dreaming, and dormant keepalive requests call
  `prepare_request` and use the same Claude Code MCP session setup as chat.
- The daemon auto-enables the loopback HTTP MCP listener when any
  `claude_code` chat model is configured.
- Claude Code `total_cost_usd`, `modelUsage`, token usage, and
  `rate_limit_event` payloads are surfaced through normalized usage telemetry.

## Known Non-Parity

### Progressive Client Streaming

Direct Anthropic/OpenRouter streaming emits chunks to clients as tokens arrive.
Claude Code currently emits Shore stream events only after the CLI turn has
completed, so clients see a quiet gap followed by the completed response. The
CLI has an `--include-partial-messages` flag, but Shore does not yet consume
that shape. Until that parser path is implemented, TUI, GUI, Matrix, and TTS
surfaces should not rely on progressive deltas for `claude_code`.

### Sampler Controls

`max_tokens`, `temperature`, and `top_p` are part of Shore's normal model
profile and API-provider request path. The Claude Code CLI help surface does
not expose equivalent non-interactive flags for the OAuth-backed path. Shore
therefore cannot faithfully forward those settings today. `reasoning_effort`
is forwarded through `--effort` because the CLI supports it.

### Anthropic Prompt Cache Controls

Direct Anthropic requests can place `cache_control` breakpoints and report cache
read/write behavior. Claude Code manages its own internal prompt/cache behavior
and the OAuth subscription path is billed as a flat plan rather than per API
call. Shore records the CLI's reported token counts and would-be API cost, but
cannot force the same `cache_ttl` placement semantics as the Anthropic API.

### Fresh-Spawn History Fidelity

Long-lived Claude Code subprocesses preserve live conversation context while
they remain alive. After daemon restart, compaction, dreaming reload, recipe
change, or subprocess death, Shore rehydrates history by flattening prior
turns into the system prompt transcript. Text, tool names, inputs, and results
remain visible, but this is not identical to replaying the Anthropic API's
native structured message history, especially for signed thinking blocks.

### CLI Isolation Surface

The cleanest Claude Code isolation flag, `--bare`, disables OAuth, so Shore
cannot use it for subscription-backed `claude_code` calls. The provider instead
uses `--setting-sources ""`, `--strict-mcp-config`, `--tools ""`,
`--no-session-persistence`, and per-session MCP URLs. This keeps Shore's tools
authoritative, but it is not byte-for-byte the same as direct API isolation.

### API-Key Fallback

Normal API providers can rotate through configured provider keys and optionally
fall back between provider entries. Claude Code has no API key on the hot path;
quota/auth failures come from the local OAuth session. Shore surfaces quota as
429-shaped telemetry, but it does not automatically fall back to a paid
Anthropic/OpenRouter key because that could surprise the user with API spend.

### Image input
Image input/multimodal chat parity looks weak: current Claude Code rendering is text/tool oriented.
