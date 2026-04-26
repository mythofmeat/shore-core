# Quirks

Things that are true in the current implementation and easy to trip over.

## Final `StreamEnd` Comes After Persistence

The final response `StreamEnd` is emitted only after the assistant message is durable. Clients should treat that event as metadata completion, not as permission to create a second message entry.

This prevents command races like `send` followed immediately by `memory compact`.

## Tool-Use Turns Have Intermediate Stream Ends

During tool use the daemon may emit:

```text
StreamStart -> chunks -> StreamEnd(tool_use) -> ToolCall -> ToolResult -> StreamStart -> ... -> StreamEnd(end_turn)
```

Clients should buffer one assistant turn across intermediate tool phases.

## Provider Thinking Blocks

Extended-thinking blocks may be required inside an in-progress tool loop but are stripped from prior completed turns by default. This saves tokens and protects cache economics.

Set `[memory.thinking].preserve_prior_turns = true` only when testing a provider that genuinely needs old thinking blocks resent.

## Active Prompt Is Not Workspace Prompt

Editable workspace files are not always prompt-active. Protected files are snapshotted into `active_prompt/`; edits queue deferred activation until compaction/reload.

If a character edits `SOUL.md` and the next normal reply still uses the old identity, that is expected.

## Heartbeat Recaps Are Memory, Not Chat

Heartbeat turns are private unless the character emits `<sendMessage>...</sendMessage>`. `HEARTBEAT_OK` is suppressed, and no daily memory note is written unless a write-capable tool does it explicitly.

## `exec` Is Useful But Narrow

The workspace `exec` tool:

- parses argv directly
- does not invoke a shell
- allows only selected executable names
- rejects executable paths
- rejects path-like arguments outside the character workspace

So `cat notes.md` works, but `cat /etc/passwd`, `git -C /tmp status`, and `cargo --manifest-path=/tmp/Cargo.toml test` are rejected.

## Memory Disabled Means `memory/...` Is Blocked

Disabling memory does more than hide `memory_*` tools. It also blocks workspace access to `memory/...` paths and hides `exec` unless memory read/write access is fully enabled.

## Matrix Live Verification Needs A Homeserver

Most Matrix bridge behavior is covered by unit/integration tests. End-to-end live verification still requires a configured embedded or external homeserver.

## Remote Bind Is Not Security

`unsafe_allow_remote_access = true` only lets the daemon bind outside loopback. `allowed_hosts` is an IP filter, not authentication. Use a private overlay network.

## MCP Defaults To A Test Profile

`shore-mcp` defaults to an isolated test profile. It only touches the main profile with `--attach-main --allow-main-writes`.

## Live Tests Cost Money

Ignored/live tests intentionally use real providers. Run them as a release gate when you mean it, not as part of ordinary local iteration.
