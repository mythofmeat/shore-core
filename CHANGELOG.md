# Changelog

## Unreleased

- Added the `claude_code` provider, which drives the local `claude` CLI through
  OAuth-backed Claude subscription usage while Shore hosts MCP tools in the
  daemon.
- Added `[daemon.http]` for the daemon-hosted MCP listener, Claude Code config
  doctor checks, usage telemetry for would-be API cost and rate-limit events,
  and an ignored live test that exercises the real CLI and MCP tool path.
- Added startup policy and documentation for non-loopback `[daemon.http]`
  exposure; the MCP listener is bearer-by-URL and has no auth or TLS.
- Serialized Claude Code keyed MCP sessions before provider dispatch so
  concurrent turns for one character cannot cross-wire tool callbacks.
- Moved Claude Code parser fixtures into tracked test data, repopulated
  `StreamResult.tool_uses` from the MCP ledger splice, and routed background
  heartbeat, compaction, dreaming, and keepalive calls through the same Claude
  Code MCP session setup.
- Hardened Claude Code subprocess handling so partial streams without a final
  `result` event fail as incomplete, and chat MCP sessions are torn down as
  soon as their tool ledger is spliced.
- Kept automatic post-turn compaction alive after one-shot CLI clients
  disconnect, so Claude Code sessions survive the compaction/reload boundary.
- Auto-enabled the daemon HTTP MCP listener when a `claude_code` chat model is
  configured, and documented remaining Claude Code parity gaps in
  `docs/claude-code-parity.md`.
- Extended Claude Code cached subprocess idle retention to one hour so native
  conversation context survives ordinary pauses between turns.
- Added progressive Claude Code client streaming via
  `--include-partial-messages`, including parser coverage for partial text
  chunks and a delayed-result forwarding test.
- Preserved current-turn image blocks in Claude Code stdin rendering and added
  live-test coverage documenting that Claude Code CLI 2.1.128 still does not
  deliver those images to the model.
- Added native Claude Code session replay for cold starts with prior Shore
  history by writing Claude JSONL session files and spawning with `--resume`;
  the live suite verifies a token present only in replayed history survives.
