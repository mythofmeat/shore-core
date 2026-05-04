# Changelog

## Unreleased

- Added the `claude_code` provider, which drives the local `claude` CLI through
  OAuth-backed Claude subscription usage while Shore hosts MCP tools in the
  daemon.
- Added `[daemon.http]` for the daemon-hosted MCP listener, Claude Code config
  doctor checks, usage telemetry for would-be API cost and rate-limit events,
  and an ignored live test that exercises the real CLI and MCP tool path.
