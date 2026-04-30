# Observability

Observability is part of the harness. Agents should be able to answer "what
happened?" from repo-local commands and files.

## Runtime Logs

Rust services use `tracing` with `RUST_LOG` / `EnvFilter`.

```sh
RUST_LOG=shore_daemon=debug,shore_llm=debug,shore_swp_server=debug shore-daemon
RUST_LOG=shore_cli=debug shore status
RUST_LOG=shore_tui=debug shore-tui
```

The TUI writes a file log at:

```text
$XDG_DATA_HOME/shore/tui.log
```

Use lower-volume filters first. Provider request bodies can include sensitive
conversation context, so do not paste logs into docs or commits.

## Command Surfaces

Useful CLI checks:

```sh
shore status
shore status --diagnostics
shore usage
shore usage --anomalies
shore log --heartbeat
```

Useful MCP checks:

- `status`
- `status_diagnostics`
- `usage`
- `log_tail`
- `log_heartbeat`
- `memory_changelog`

`shore-mcp` defaults to an isolated test profile, which makes it the preferred
agent end-to-end harness.

## Persistent Diagnostics

| Surface | Location | Purpose |
| --- | --- | --- |
| Usage ledger | `$XDG_DATA_HOME/shore/ledger.db` | LLM calls, usage, cost, cache reads/writes, anomalies |
| Cache forensics | `$XDG_DATA_HOME/shore/cache_forensics.jsonl` | Anthropic cache placement and response events when enabled |
| Conversation log | `$XDG_DATA_HOME/shore/<Character>/active.jsonl` | Active daemon-owned conversation |
| Compacted segments | `$XDG_DATA_HOME/shore/<Character>/segments/` | Archived older conversation messages |
| Active prompt snapshot | `$XDG_DATA_HOME/shore/<Character>/active_prompt/` | Prompt-active protected files |
| Deferred prompt edits | `$XDG_DATA_HOME/shore/<Character>/deferred_edits.jsonl` | Protected workspace edits waiting for activation |
| Dreaming state | `$XDG_DATA_HOME/shore/<Character>/dreams/` | Scheduler timestamps and machine-readable dreaming artifacts |

Enable cache forensics with:

```toml
[advanced]
cache_forensics = true
```

## Test Harness Signals

`dev/test-harness` exposes helpers for:

- booting a real daemon in-process with a mock LLM;
- collecting full streamed responses;
- triggering compaction deterministically;
- simulating crash/restart paths;
- reading ledger rows from the isolated test data directory.

When a bug is hard to see, prefer adding a deterministic harness assertion over
manual log inspection.

## Handoff Rule

If validation depends on inspecting a diagnostic surface, mention the exact
command or file in the handoff. Do not rely on memory of terminal output.
