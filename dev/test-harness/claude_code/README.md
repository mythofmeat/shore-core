# Claude Code Provider Harness

Run the deterministic regression set:

```sh
dev/test-harness/claude_code/run.sh
```

Run the same checks plus the real Claude CLI live tests:

```sh
SHORE_CLAUDE_CODE_LIVE=1 dev/test-harness/claude_code/run.sh
```

Live mode requires `claude` on `PATH`, `claude auth login`, and enough remaining
Claude subscription quota. The harness starts
`dev/spikes/claude-code-probe/mcp_http_server.py` on loopback and tears it down
after `cargo test -p shore-llm --test claude_code_live -- --ignored`.
