# shore-mcp

`shore-mcp` exposes Shore's daemon through MCP for development and agent-driven verification.

It is not a separate Shore backend. It speaks to a Shore daemon using the same SWP path as the CLI/TUI.

## Profiles

Default mode uses an isolated persistent test profile:

```text
$XDG_DATA_HOME/shore-mcp-test/
```

Modes:

| Mode | Flags | Writes allowed |
| --- | --- | --- |
| persistent test | default | yes |
| ephemeral test | `--ephemeral` | yes, tempdir only |
| main read-only | `--attach-main` | no mutating tools |
| main writable | `--attach-main --allow-main-writes` | yes |

Use main writable mode only when you explicitly intend to mutate the real profile.

## Daemon Handling

In test-profile modes, `shore-mcp` discovers or spawns a `shore-daemon` with a stable `--instance-id`. `--daemon-addr` can target an existing daemon.

## Tool Surface

The MCP surface mirrors CLI/daemon operations:

- status/config/model/character/usage/log
- send and regen
- memory query/compact/status
- debug heartbeat commands

Mutating tools are gated by profile mode.

## Current Memory Model

Memory operations target markdown memory under each character workspace:

```text
characters/<Character>/workspace/memory/
```

The old SQLite/vector/RAG memory stack is not the active runtime target.

## Verification Use

`shore-mcp` is the quickest way to drive an end-to-end daemon path from an agent:

1. start `shore-mcp`
2. send MCP `initialize`
3. list tools
4. call `send`, `memory_compact`, `status`, etc.

Live model calls require configured provider keys and may cost money.
