# dev/mcp-servers

First-party MCP servers shipped alongside Shore for the **outbound** plugin
surface. The daemon spawns these as configured under `[mcp.servers.<name>]`
in `config.toml` and exposes their tools to characters as
`mcp__<name>__<tool>` after the allowlist + `destructiveHint` policy filter
passes.

These servers are intentionally minimal — they wrap an existing data source
(local API, library, etc.) over the MCP stdio transport. They share no code
with each other; each is a leaf script with its own dependencies.

| Server | Purpose |
| --- | --- |
| [`hello/`](hello/) | Single-tool sanity check used to verify daemon plumbing end-to-end. Not useful in real conversations. |

For the architecture and privilege model, see
[`../../ARCHITECTURE.md`](../../ARCHITECTURE.md). For configuration syntax
see [`../../CONFIGURATION.md`](../../CONFIGURATION.md).

## Adding a server

1. Create `dev/mcp-servers/<name>/` with the server script and a brief
   `README.md` covering tools, env vars, and a working `[mcp.servers.<name>]`
   config snippet.
2. Add it to the table above.
3. The daemon does not depend on Python — bring your own runtime. The
   spawn `command` and `args` in user config decide what gets executed.
