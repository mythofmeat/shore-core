# Shore Logging Guide

Shore uses [`tracing`](https://docs.rs/tracing) throughout. All log output is controlled by the `RUST_LOG` environment variable — no config file, no code changes needed.

---

## Quick start

```sh
RUST_LOG=shore_daemon=debug ./shore-daemon
```

---

## Log levels

| Level   | What you see |
|---------|--------------|
| `error` | Data loss, broken requests, cache anomalies detected by the ledger |
| `warn`  | Retries, fallbacks, pricing fetch failures, unexpected cache state, parse errors |
| `info`  | Lifecycle events: generation start/end, compaction, connections, pricing refresh |
| `debug` | Per-request detail: tool iterations, stream open/close, DB ops, model resolution |
| `trace` | Wire-level: per-chunk streaming bytes |

**Recommended starting point for debugging:** `debug` on the crate you care about, `warn` on everything else.

---

## Filtering by crate

```sh
# Daemon only
RUST_LOG=shore_daemon=debug ./shore-daemon

# Daemon + ledger (cost tracking, cache state)
RUST_LOG=shore_daemon=debug,shore_ledger=debug ./shore-daemon

# LLM provider calls only
RUST_LOG=shore_llm_client=debug ./shore-daemon

# Everything at info, daemon at debug
RUST_LOG=info,shore_daemon=debug ./shore-daemon
```

## Filtering by module

```sh
# Only the handler hot path (generation, retries, tool loop)
RUST_LOG=shore_daemon::handler=debug ./shore-daemon

# Only the memory subsystem
RUST_LOG=shore_daemon::memory=debug ./shore-daemon

# Only autonomy (ticks, interiority, activity)
RUST_LOG=shore_daemon::autonomy=debug ./shore-daemon

# Only compaction
RUST_LOG=shore_daemon::memory::compaction=debug ./shore-daemon

# Only pricing and cache tracking in the ledger
RUST_LOG=shore_ledger::pricing=debug,shore_ledger::cache_tracker=debug ./shore-daemon
```

---

## Span context (`#[instrument]`)

Key async functions are annotated with `#[instrument]`, which creates a tracing span that nested log lines inherit. This means you can see which generation, character, and model produced every log line without adding that context manually everywhere.

**Instrumented functions and their span fields:**

| Function | Span fields |
|---|---|
| `handler::handle_generation` | `char`, `rid` |
| `handler::stream_with_retry` | `char`, `model` |
| `handler::run_tool_phase` | `char` |
| `handler::persist_and_notify` | `char`, `model` |
| `engine::tools::run_tool_loop` | `char`, `max_iterations` |
| `memory::agent::ask` | `caller`, `model`, `question_len` |
| `memory::agent::run_agent_loop` | `model` |
| `memory::compaction::compact` | `char`, `user`, `msg_count`, `dry_run` |
| `memory::vectorstore::open` | `path`, `dimension` |
| `memory::vectorstore::search` | `top_k` |
| `ledger::client::record_call` | `call_type` |
| `ledger::client::generate` | `model`, `call_type` |
| `ledger::client::stream_raw` | `model`, `call_type` |
| `ledger::pricing::fetch_pricing` | _(default)_ |
| `ledger::pricing::get_or_fetch` | _(default)_ |
| `run::execute` (CLI) | _(default)_ |
| `main::run_tui` (TUI) | _(default)_ |

**Example output** with `RUST_LOG=shore_daemon=debug`:

```
DEBUG shore_daemon::handler{char="aria" rid="m_abc123"}:handle_generation: handle_generation starting regen=false text_len=42
DEBUG shore_daemon::handler{char="aria" rid="m_abc123"}:stream_with_retry{model="anthropic/claude-opus-4-6"}: stream_with_retry starting max_retries=3
INFO  shore_daemon::handler{char="aria" rid="m_abc123"}:stream_with_retry{model="anthropic/claude-opus-4-6"}: Response complete input_tokens=1820 output_tokens=312
DEBUG shore_daemon::engine::tools{char="aria" max_iterations=8}: Tool loop iteration iteration=1 tool_count=2
```

---

## Common debugging scenarios

### "Why is this generation slow?"
```sh
RUST_LOG=shore_daemon::handler=debug,shore_ledger=info ./shore-daemon
```
Look for `stream_with_retry` span — it will show retry attempts and backoff delays.

### "Is the cache working?"
```sh
RUST_LOG=shore_ledger::cache_tracker=debug,shore_ledger::client=info ./shore-daemon
```
The cache state machine logs every transition (cold→warm, warm→cold on TTL/model change) and flags anomalies at `error` level.

### "What is the memory agent doing?"
```sh
RUST_LOG=shore_daemon::memory::agent=debug ./shore-daemon
```
Shows each tool call the agent makes, read vs. write ops per iteration, and the final mutation list.

### "Why did compaction fire / not fire?"
```sh
RUST_LOG=shore_daemon::memory::compaction=debug,shore_daemon::autonomy=debug ./shore-daemon
```

### "What model/pricing is being used?"
```sh
RUST_LOG=shore_config=debug,shore_ledger::pricing=debug ./shore-daemon
```

### Broad sweep (verbose but complete)
```sh
RUST_LOG=debug ./shore-daemon 2>&1 | tee shore.log
```
