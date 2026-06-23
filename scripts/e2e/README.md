# scripts/e2e — runnable, isolated e2e test daemon

`daemon.sh` spins up a **real** `shore-daemon` (your current local build) in a
throwaway profile so you can exercise a feature end-to-end — arbitrary config
including `[mcp.*]` servers and sub-agents, real models — **without touching your
live daemon**, its conversation, memory, port, or LLM-sidecar socket.

Use it when you need to see a change actually work against real providers/servers.
For deterministic, in-CI coverage (including MCP tools and sub-agents driven by a
mock LLM), use the Rust harness instead: `dev/test-harness` +
`backend/daemon/tests/suite/` (e.g. `autonomy.rs` already e2e-tests a sub-agent
owning `mcp__*` tools), and `backend/mcp/tests/stdio.rs` for the MCP client.

## Isolation guarantees

| concern | how it's isolated |
|---|---|
| config / data / runtime | own temp `SHORE_{CONFIG,DATA,RUNTIME}_DIR` (`core/config/src/lib.rs:163`) |
| LLM sidecar socket | `runtime.join("llm.sock")` (`backend/daemon/src/main.rs:578`) → isolated runtime ⇒ isolated socket |
| port | binds `127.0.0.1:0`; the CLI only ever connects via explicit `--addr`, never discovery |
| heartbeats | `[behavior.autonomy] enabled = false` by default |
| Matrix (port 6167) | not configured unless your `--config` adds it |
| provider keys | a copy of your real `~/.config/shore/.env` + `conf.d/*.toml` |

The harness writes `conf.d/_e2e_override.toml` last, and conf.d deep-merges **over**
the main config (`core/config/src/lib.rs:415`), so the isolated `addr`, primary
`model`, and autonomy-off always win regardless of the caller config.

## Commands

```sh
daemon.sh up   [--name N] [--config FILE] [--character FILE] [--model REF] [--release]
daemon.sh send [--name N] "message"
daemon.sh exec [--name N] -- <shore args...>   # any shore subcommand vs the instance
daemon.sh logs [--name N]                      # tail the daemon log
daemon.sh list                                 # running e2e instances
daemon.sh down [--name N]                       # stop + delete the instance
```

- `--config` may carry `[mcp.*]`, `[subagents.*]`, `[tools]`, etc. Omit it for a
  bare character.
- `--model` sets the primary chat model (default `opencode-go:glm-5.2`, a $0
  subscription model). It must resolve via the copied providers.
- `--release` builds/uses the release profile; default is `debug` (tests local code).
- `--name` runs multiple instances side by side (default `default`).

## Example — verify `ask_music` end-to-end

**Setup:** the example points at an external MCP server, `mcp-listening-stats`
(beets + ListenBrainz). It is not vendored here — check it out separately, then
edit the `cwd` in `examples/music.toml` to your local path (`uv run` resolves the
project from that directory; it's required because the daemon runs in a temp
profile). The same pattern applies to any `[mcp.*]` server you point the harness
at: install it, then set its `cwd`/`command`/`args` in your `--config`.

```sh
scripts/e2e/daemon.sh up --name music \
    --config    scripts/e2e/examples/music.toml \
    --character scripts/e2e/examples/music-soul.md
scripts/e2e/daemon.sh send --name music \
    "What have I played most this month, and what do critics say about it?"
scripts/e2e/daemon.sh exec --name music -- log     # confirm mcp__listening_stats__* + web tool calls
scripts/e2e/daemon.sh down --name music
```

The hue server is wired the same way (`[mcp.hue]` + a `lights` sub-agent); it's left
out of the auto-run example because its tools change physical lights — copy the
`listening_stats` block and point a sub-agent at `mcp__hue__*` to try it.

## Reproducing the old shell scripts

This harness replaces `test-daemon.sh`, `live-tests/{smoke,live,autonomy}-test.sh`,
and the exploratory `cache-tests/{19,experiment,20-23}` probes:

- **CLI smoke** (status, character, no-color, bad addr): `daemon.sh exec -- status`,
  `daemon.sh exec -- character`, etc.
- **Live API turn**: `daemon.sh send "hi"`.
- **Autonomy / heartbeat**: bring an instance up, then
  `daemon.sh exec -- debug heartbeat_tick_now` and inspect `daemon.sh exec -- status`
  / `daemon.sh logs`. (Override autonomy-off via your `--config` if you want the
  deadline-based loop.)

Cache economics now live in Rust: `dev/test-harness/tests/live_cache_regression.rs`
and `live_compaction_cache.rs` (gated `#[ignore]`, real OpenRouter). The 24h idle
keepalive soak remains at `scripts/cache-tests/keepalive-24h.sh`.
