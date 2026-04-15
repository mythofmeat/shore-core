# shore-mcp

An MCP (Model Context Protocol) server that exposes Shore's CLI surface to AI
clients — primarily Claude Code — for debugging and programmatic use.

## Build gate

`shore-mcp` is **debug-only by default**. Three layers prevent it from being
built unintentionally:

1. The `[[bin]]` declares `required-features = ["enabled"]`.
2. The `enabled` feature pulls in the optional `rmcp` and `schemars` deps.
3. `main.rs` is `cfg(debug_assertions)`-gated, so even a `--release` build
   without an opted-in custom profile produces no binary.

`cargo build --workspace --release` will not produce `shore-mcp`. That is
intentional. See [docs/DECISIONS.md](../docs/DECISIONS.md) entry
"shore-mcp crate added as a debug-only MCP server" for the rationale.

## Build & run

Workspace aliases (defined in `.cargo/config.toml`):

```sh
cargo mcp           # build
cargo mcp-run       # run with no args (default test profile + spawn)
cargo mcp-test      # unit tests
cargo mcp-itest     # integration test against shore-test-harness
cargo mcp-check     # type-check including tests
```

Manual equivalents are `cargo build -p shore-mcp --features enabled`, etc.

## Profile modes

`shore-mcp` chooses its target Shore daemon at startup. There are three modes:

| Flag(s)             | Profile             | Daemon                                             | Mutation tools |
| ------------------- | ------------------- | -------------------------------------------------- | -------------- |
| _(default)_         | persistent test     | discovered or spawned at `shore-mcp-test`          | allowed        |
| `--ephemeral`       | fresh tempdir       | spawned, torn down on exit                         | allowed        |
| `--attach-main`     | user's real profile | discovered via normal `shore-client` discovery     | **refused**    |
| `--attach-main --allow-main-writes` | same        | same                                               | allowed        |

`--allow-main-writes` is a deliberate two-flag opt-in. Without it, mutation
tools (`send`, `regen`, `config_set`, `character_switch`, etc.) refuse with a
gate-refuse message instead of executing against the user's main profile.

`--daemon-addr ADDR` overrides discovery and spawning — used by the
integration test to point at an in-process daemon.

## .mcp.json example

For Claude Code, drop a fragment like this in your project's `.mcp.json`:

```jsonc
{
  "mcpServers": {
    "shore": {
      "command": "/abs/path/to/silvershore/target/debug/shore-mcp",
      "args": []
    }
  }
}
```

To target your real Shore profile (read-only):

```jsonc
{
  "mcpServers": {
    "shore-main": {
      "command": "/abs/path/to/silvershore/target/debug/shore-mcp",
      "args": ["--attach-main"]
    }
  }
}
```

To target your real Shore profile with mutation tools enabled:

```jsonc
{
  "mcpServers": {
    "shore-main-rw": {
      "command": "/abs/path/to/silvershore/target/debug/shore-mcp",
      "args": ["--attach-main", "--allow-main-writes"]
    }
  }
}
```

## Tool surface

Tools are grouped by category. Read-only tools always run; mutating tools obey
the gate described above.

### Read-only

| Tool                 | Purpose                                       |
| -------------------- | --------------------------------------------- |
| `status`             | Daemon process / connection status            |
| `status_diagnostics` | Diagnostics counters                          |
| `log_tail`           | Tail of the active conversation log          |
| `log_show`           | Full message body by id                       |
| `log_heartbeat`      | Recent heartbeat ticks                        |
| `log_follow`         | Bounded follow with timeout (read tool)       |
| `usage`              | Token / cost accounting                       |
| `config_get`         | Read a config key                             |
| `config_check`       | Validate the current config                   |
| `character_list`     | List installed characters                     |
| `character_info`     | Detail on one character                       |
| `model_list`         | List available models                         |
| `model_info`         | Detail on one model                           |
| `memory_query`       | Query the vector memory                       |
| `memory_changelog`   | Memory writes over a recent window            |

### Mutating (gated)

| Tool                    | Purpose                                       |
| ----------------------- | --------------------------------------------- |
| `send`                  | Send a user message; returns the full reply   |
| `regen`                 | Regenerate the last assistant turn            |
| `log_delete`            | Delete a message by id                        |
| `log_edit`              | Edit a message by id                          |
| `config_set`            | Set a config key                              |
| `config_reset`          | Reset config to defaults                      |
| `character_switch`      | Switch the active character                   |
| `model_switch`          | Switch the active model                       |
| `model_reset`           | Reset model to default                        |
| `memory_compact`        | Run memory compaction                         |
| `memory_collate`        | Run memory collation                          |
| `memory_purge`          | Purge memory entries                          |
| `memory_reindex`        | Reindex the vector store                      |
| `debug_tick_now`        | Force an interiority tick                     |
| `debug_status_dormant`  | Force the agent into the dormant phase        |
| `debug_status_active`   | Force the agent into the active phase         |

## Integration test

`shore-mcp/tests/mcp_integration.rs` spawns the binary as a subprocess against
an in-process daemon booted by `shore-test-harness`, drives MCP JSON-RPC over
stdio, and verifies `initialize` / `tools/list` / `status` / `send` end-to-end
through a mock LLM. It is gated behind `--ignored`:

```sh
cargo mcp-itest
```
