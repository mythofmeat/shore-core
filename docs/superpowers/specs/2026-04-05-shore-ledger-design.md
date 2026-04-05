# shore-ledger: Token Usage Tracking & Cache Anomaly Detection

**Date:** 2026-04-05
**Status:** Approved design

## Problem

Shore makes many LLM calls across multiple providers and call types (messages, tool loops, keepalives, interiority, compaction, memory agent, researcher). There is no persistent record of token consumption or cost. The Anthropic provider is the most expensive, and Anthropic's own logging does not provide per-call granularity or cache hit/miss visibility. Without a ledger, it is impossible to answer: "How much did I spend today?", "Is caching working?", or "Which call type is burning the most money?"

## Solution

A new `shore-ledger` crate that provides:

1. **Compiler-enforced logging** via a `LedgerClient` wrapper that consumes `LlmClient`
2. **Persistent SQLite ledger** of every LLM call with full token counts and calculated costs
3. **Cache state tracker** that detects anomalies in Anthropic's prompt caching
4. **Pricing engine** backed by OpenRouter's model API with local DB fallback
5. **CLI interface** for querying usage, costs, and anomalies

## Architecture

### Crate: `shore-ledger`

New workspace crate. Dependencies: `shore-llm-client`, `rusqlite`, `reqwest` (for pricing), `tracing`, `chrono`.

Does NOT depend on `shore-daemon`. The daemon depends on `shore-ledger`.

### Components

```
shore-ledger/src/
  lib.rs              — public API: LedgerClient, CallType
  client.rs           — LedgerClient: wraps LlmClient, enforces recording
  ledger.rs           — Ledger: SQLite append + query
  cache_tracker.rs    — CacheTracker: per-character warm/cold state machine
  pricing.rs          — PricingEngine: OpenRouter fetch + DB cache + overrides
  query.rs            — Query helpers for CLI (aggregation, filtering, export)
```

## LedgerClient

The daemon constructs a `LedgerClient` at startup. The raw `LlmClient` is consumed and inaccessible — compiler-enforced, not convention.

```rust
pub struct LedgerClient {
    inner: LlmClient,
    ledger: Ledger,
    cache_tracker: Mutex<HashMap<String, CacheTracker>>,  // keyed by character
    pricing: PricingEngine,
}

impl LedgerClient {
    /// Consumes the LlmClient. No way to get it back.
    pub fn new(client: LlmClient, db_path: &Path) -> Result<Self>;

    /// Passthrough — no recording needed for request construction.
    pub fn build_request(...) -> Result<LlmRequest>;

    /// Streams, then records to ledger on stream completion.
    /// Returns a wrapped stream that auto-records when the `done` event is read.
    pub async fn stream_raw(
        &self,
        request: &LlmRequest,
        call_type: CallType,
        character: &str,
    ) -> Result<(LedgerBufReader, RequestId)>;

    /// Generates, records, returns.
    pub async fn generate(
        &self,
        request: &LlmRequest,
        call_type: CallType,
        character: &str,
    ) -> Result<GenerateResponse>;
}
```

### CallType

```rust
pub enum CallType {
    Message,       // user-initiated message
    ToolLoop,      // tool call iteration within a tool loop
    Keepalive,     // cache keepalive ping
    Interiority,   // internal reflection tick
    Compaction,    // message compaction/summarization
    MemoryAgent,   // background memory operations
    Researcher,    // semantic search / embeddings
}
```

### LedgerBufReader (Stream Wrapper)

When `stream_raw` is called, the `LedgerClient`:
1. Calls the inner `LlmClient::stream_raw()` to get the raw stream
2. Wraps the returned reader in a `LedgerBufReader` that intercepts the NDJSON `done` event
3. When `done` is read, atomically: records the row to the DB, updates cache tracker, calculates cost
4. Returns the wrapped reader — callers (StreamConsumer) use it identically to the raw reader

This guarantees that stream consumption = ledger recording. No separate call needed.

## SQLite Schema

**Database location:** `$XDG_DATA_HOME/shore/ledger.db` (typically `~/.local/share/shore/ledger.db`)

### `calls` table

```sql
CREATE TABLE calls (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                  TEXT NOT NULL,               -- ISO 8601 UTC
    character           TEXT NOT NULL,
    provider            TEXT NOT NULL,               -- "anthropic", "openai", "zai", etc.
    model               TEXT NOT NULL,
    call_type           TEXT NOT NULL,               -- "message", "tool_loop", "keepalive", etc.
    input_tokens        INTEGER NOT NULL,
    output_tokens       INTEGER NOT NULL,
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0,
    total_ms            INTEGER NOT NULL,
    ttft_ms             INTEGER NOT NULL DEFAULT 0,
    finish_reason       TEXT NOT NULL,
    thinking_enabled    INTEGER NOT NULL DEFAULT 0,  -- 0 or 1
    cache_state         TEXT,                        -- "warm", "cold", NULL (non-Anthropic)
    cache_anomaly       TEXT,                        -- NULL, "unexpected_read", "unexpected_write"
    input_cost          REAL,                        -- USD, NULL if pricing unavailable
    output_cost         REAL,
    cache_read_cost     REAL,
    cache_write_cost    REAL,
    total_cost          REAL
);

CREATE INDEX idx_calls_ts ON calls(ts);
CREATE INDEX idx_calls_character ON calls(character);
CREATE INDEX idx_calls_provider ON calls(provider);
CREATE INDEX idx_calls_anomaly ON calls(cache_anomaly) WHERE cache_anomaly IS NOT NULL;
```

### `pricing` table

```sql
CREATE TABLE pricing (
    model_id            TEXT PRIMARY KEY,            -- "anthropic/claude-opus-4-6"
    input_per_token     REAL NOT NULL,               -- USD per token
    output_per_token    REAL NOT NULL,
    cache_read_per_token  REAL NOT NULL DEFAULT 0,
    cache_write_per_token REAL NOT NULL DEFAULT 0,
    fetched_at          TEXT NOT NULL                 -- ISO 8601 UTC
);
```

## Cache Tracker

Per-character state machine for Anthropic models. Stored in memory, reconstructed from DB on daemon startup.

### States

- **Cold** — no valid cache. Expect next call to be a full cache write.
- **Warm** — cache is active. Expect cache_read to monotonically increase.

### Transitions

```
Cold  ──[response: cache_write > 0, cache_read = 0]──▶  Warm    (expected)
Warm  ──[cache_ttl expired]───────────────────────────▶  Cold
Warm  ──[compaction event]────────────────────────────▶  Cold    (expected, not anomaly)
Warm  ──[model changed]──────────────────────────────▶  Cold
Warm  ──[thinking toggled on/off]─────────────────────▶  Cold
```

### Anomaly Detection

On each Anthropic API response:

1. Check if warm state has expired: `now - last_call_ts > cache_ttl` → transition to Cold
2. Check if model or thinking-enabled status changed vs last call → transition to Cold
3. Check if this is a compaction event → transition to Cold (expected)
4. Compare actual behavior against expected:

| State | Condition | Verdict |
|-------|-----------|---------|
| Warm  | `cache_read >= prev_cache_read` | OK — stay Warm |
| Warm  | `cache_read < prev_cache_read` | **ANOMALY: `unexpected_write`** — cache partially/fully invalidated |
| Cold  | `cache_read = 0`, `cache_write > 0` | OK — transition to Warm |
| Cold  | `cache_read > 0` | **ANOMALY: `unexpected_read`** — our Cold logic is wrong |

### Anomaly Response

When an anomaly is detected:
1. `tracing::error!` — appears in systemd journal
2. Recorded in `cache_anomaly` column of the DB row
3. Fired through the existing notification service (if enabled)

### Startup Reconstruction

```sql
SELECT ts, model, thinking_enabled, cache_read_tokens, cache_write_tokens
FROM calls
WHERE character = ? AND provider = 'anthropic' AND call_type != 'compaction'
ORDER BY ts DESC LIMIT 1
```

If that row is younger than `cache_ttl` and has `cache_read_tokens > 0` → start Warm with that row's `cache_read_tokens` as baseline. Otherwise → start Cold.

## Pricing Engine

### Data Source

OpenRouter's public `/api/v1/models` endpoint. No auth required. Returns per-token pricing for nearly every model across all providers.

### Model ID Mapping

Our `(provider, model)` tuple maps to OpenRouter's format as `"{provider}/{model}"`. For the vast majority of models this is a direct match. A small hardcoded override table handles any divergences.

### Fetch Strategy

- **Lazy per-model:** First time a model is seen, fetch its pricing from OpenRouter
- **DB cache:** Store in `pricing` table. Subsequent calls use DB row, no fetch
- **Fallback:** If fetch fails and DB row exists → use stale DB row + log warning. If no DB row → costs are NULL + log warning
- **No automatic TTL:** Prices change rarely. Manual refresh via `shore usage --refresh-pricing`

### Anthropic 1h Cache TTL Override

OpenRouter reports 5-minute cache TTL pricing. For Anthropic models using the 1-hour cache TTL (which shore uses), cache_read and cache_write prices are different. These are hardcoded overrides applied on top of the OpenRouter base pricing when the provider is `"anthropic"`.

## CLI Interface

New subcommand on the `shore` binary:

```
shore usage                                  # today's costs by model
shore usage --last 7d                        # last 7 days
shore usage --last 30d --character aria       # filtered by character
shore usage --provider anthropic             # filtered by provider
shore usage --model claude-opus-4-6          # filtered by model
shore usage --call-type keepalive            # filtered by call type
shore usage --anomalies                      # cache anomalies only
shore usage --export-csv                     # full ledger as CSV to stdout
shore usage --export-tsv                     # full ledger as TSV to stdout
shore usage --refresh-pricing                # clear cached pricing
shore usage --recalculate                    # backfill NULL costs with current pricing
```

### Default Summary Output

```
Shore Usage — 2026-04-05

Provider     Model                 Calls  Input     Output    Cache R   Cache W   Cost
anthropic    claude-opus-4-6       47     125.2K    18.3K     112.8K    12.4K     $4.82
anthropic    claude-haiku-4-5      12     8.1K      2.4K      6.9K      1.2K      $0.03
openai       gpt-4o                3      4.2K      1.1K      —         —         $0.02
                                                                          Total:  $4.87

Cache Health (anthropic):
  aria    — Warm (last hit: 2m ago, streak: 23 calls)
  test    — Cold (last call: 3h ago, expired)

Anomalies (last 7d): 0
```

## Integration Points

### Daemon Changes

1. **Startup:** Construct `LedgerClient::new(llm_client, db_path)` instead of raw `LlmClient`
2. **All LLM call sites** must pass `CallType` and character name:
   - `handler.rs` — Message, ToolLoop, Keepalive
   - `autonomy/manager.rs` — Interiority
   - `memory/compaction_impls.rs` — Compaction
   - `memory/agent_llm.rs` — MemoryAgent
   - `memory/researcher.rs` — Researcher
3. **Notifications** — cache anomalies fire through existing notification service

### What Does NOT Change

- `shore-llm-client` internals — providers, stream parsing, `Usage` struct unchanged
- `shore-diagnostics` — in-memory ring buffer for real-time SWP broadcasting, unchanged
- `shore-protocol` — no wire protocol changes
- `shore-config` — no config changes

### Dependency Graph

```
shore-daemon ──▶ shore-ledger ──▶ shore-llm-client
shore-cli    ──▶ shore-ledger (query only, no LlmClient needed)
```

## Future Work (Out of Scope)

- TUI integration (live cost display in shore-tui)
- Budget alerts / spending caps
- Per-conversation cost tracking
- Automatic pricing refresh on a schedule
- Migration from SQLite to a different backend
