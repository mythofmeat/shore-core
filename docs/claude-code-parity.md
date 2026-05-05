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
- Client streaming consumes Claude Code `--include-partial-messages` events and
  forwards text/thinking deltas as they arrive. Completed final assistant text
  blocks are suppressed when they would duplicate partial chunks; tool-use
  blocks are still preserved from the final assistant event.
- Current-turn Shore image blocks are preserved in the Claude Code stdin frame
  instead of being flattened away, so the provider will forward the same
  Anthropic-style base64 image shape used by other Claude-family paths.
- Cold starts with prior Shore history synthesize a native Claude Code JSONL
  session file and spawn with `--resume <session_id>`, avoiding the old
  system-prompt transcript fallback for normal text/tool history. This path is
  live-tested with a token present only in replayed history.

## Known Non-Parity

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

### CLI Isolation Surface

The cleanest Claude Code isolation flag, `--bare`, disables OAuth, so Shore
cannot use it for subscription-backed `claude_code` calls. The provider instead
uses `--setting-sources ""`, `--strict-mcp-config`, `--tools ""`,
`--no-session-persistence` for fresh non-resume starts, and per-session MCP
URLs. Native session replay intentionally omits `--no-session-persistence` for
the resumed subprocess after Shore has rewritten the target session JSONL. This
keeps Shore's tools authoritative, but it is not byte-for-byte the same as
direct API isolation.

### Native Session Replay Limits

The JSONL replay path is intentionally synthetic. It maps Shore user/system and
assistant messages into the Claude Code session shape that `--resume` accepts,
but Claude Code may change this undocumented format, and signed thinking blocks
from older history are only as faithful as the persisted Shore blocks allow.
The provider keeps the previous system-prompt transcript fallback available via
`provider_options.native_session_replay = false`.

### API-Key Fallback

Normal API providers can rotate through configured provider keys and optionally
fall back between provider entries. Claude Code has no API key on the hot path;
quota/auth failures come from the local OAuth session. Shore surfaces quota as
429-shaped telemetry, but it does not automatically fall back to a paid
Anthropic/OpenRouter key because that could surprise the user with API spend.

### Image Input

Image input remains non-parity in Claude Code CLI 2.1.128. Shore now preserves
current-turn Anthropic-style base64 image blocks, and the CLI accepts that
stream-json frame syntactically, but a live red-pixel probe on 2026-05-05
returned that Claude could not see the image. The documented `--file` flag is
not a local upload path; it expects Claude-hosted `file_id:relative_path`
resources and requires `CLAUDE_CODE_SESSION_ACCESS_TOKEN`. There is no
documented local `--image`/attach flag in the current `claude --help` surface.
