# claude-code-probe

Phase-0 spike for the Claude-Code-as-shore-LLM-backend design. Answers
yes/no questions about whether `claude -p` can be driven as a clean
chat backend that delegates tool calls to shore-defined tools (via
MCP) instead of running its own agent loop with built-in tools.

This is throwaway code. It does not link against shore. Findings get
hand-rolled into FINDINGS.md and the architecture decision goes to
shore proper after that.

## What we want answered

1. Does `--system-prompt` fully replace the default Claude Code system
   prompt? (We need shore's character prompt to be the only prompt.)
2. Does `--tools ""` disable every built-in tool? (The model must not
   be able to invoke Bash/Read/Edit/etc. on the proxy host.)
3. Does an MCP-registered tool actually round-trip — model emits a
   tool call, our server runs, result is fed back, model uses it?
4. Can prior-turn `thinking` blocks be replayed via `--input-format
   stream-json`?
5. How do we isolate per-character state — `HOME` override,
   `--setting-sources`, something else?
6. What do `stream-json` input/output frames actually look like? We
   need the schema before writing a Rust provider.

## Layout

- `mcp_ping.py` — tiny stdio MCP server with one tool, `ping`. Logs
  every request it gets to `results/mcp-ping.log`.
- `mcp-config.json` — wires up `ping` for `--mcp-config`.
- `probes/NN-*.sh` — one probe per question. Each writes raw output
  under `results/`.
- `run.sh` — runs probe scripts in order.
- `FINDINGS.md` — hand-written summary at the end.

## Running

```sh
./run.sh         # runs all probes
./probes/01-system-prompt.sh   # one at a time
```

Each probe is read-only on the user's home directory (uses
`--no-session-persistence`). Probe 05 sets `HOME` to a tempdir to
test isolation.
