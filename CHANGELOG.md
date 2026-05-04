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
