# shore-ledger Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Compiler-enforced token usage logging with cache anomaly detection and cost tracking across all LLM providers.

**Architecture:** A new `shore-ledger` crate wraps `LlmClient` in a `LedgerClient` that records every LLM call to a SQLite database. A `CacheTracker` state machine monitors Anthropic prompt cache health and raises errors on anomalies. A `PricingEngine` fetches per-model pricing from OpenRouter's API. The CLI gains a `shore usage` subcommand for querying costs and anomalies.

**Tech Stack:** Rust, rusqlite (bundled), reqwest, tokio, chrono, clap

**Spec:** `docs/superpowers/specs/2026-04-05-shore-ledger-design.md`

---

## File Structure

```
shore-ledger/
  Cargo.toml
  src/
    lib.rs              — Public API: re-exports LedgerClient, CallType, LedgerStream, Ledger, CacheTracker, PricingEngine, query
    client.rs           — LedgerClient struct, wraps LlmClient
    stream.rs           — LedgerStream struct, wraps BufReader<DuplexStream>
    ledger.rs           — Ledger struct: SQLite schema, insert, raw query
    cache_tracker.rs    — CacheTracker: per-character warm/cold state machine
    pricing.rs          — PricingEngine: OpenRouter fetch, DB cache, Anthropic overrides
    query.rs            — Aggregation/filter queries for CLI, CSV/TSV export
```

**Modified files:**
- `Cargo.toml` (workspace root) — add shore-ledger member + workspace dep, promote rusqlite to workspace dep
- `shore-daemon/Cargo.toml` — add shore-ledger dep, use workspace rusqlite
- `shore-daemon/src/main.rs` — construct LedgerClient instead of LlmClient
- `shore-daemon/src/handler.rs` — change LlmClient → LedgerClient, pass CallType + character
- `shore-daemon/src/engine/tools.rs` — pass CallType::ToolLoop + character
- `shore-daemon/src/autonomy/manager.rs` — pass CallType::Interiority/Keepalive + character
- `shore-daemon/src/memory/agent_llm.rs` — add character field, pass CallType::MemoryAgent
- `shore-daemon/src/memory/compaction_impls.rs` — add character field, pass CallType::Compaction
- `shore-daemon/src/memory/collation_impls.rs` — add character field, pass CallType::MemoryAgent
- `shore-daemon/src/memory/researcher.rs` — character available via AgentLlm impl
- `shore-cli/Cargo.toml` — add shore-ledger dep
- `shore-cli/src/cli.rs` — add Usage subcommand
- `shore-cli/src/main.rs` or `run.rs` — handle Usage locally (no daemon needed)

---

### Task 1: Scaffold shore-ledger crate

**Files:**
- Create: `shore-ledger/Cargo.toml`
- Create: `shore-ledger/src/lib.rs`
- Create: `shore-ledger/src/ledger.rs`
- Create: `shore-ledger/src/cache_tracker.rs`
- Create: `shore-ledger/src/pricing.rs`
- Create: `shore-ledger/src/client.rs`
- Create: `shore-ledger/src/stream.rs`
- Create: `shore-ledger/src/query.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "shore-ledger"
version = "0.1.0"
edition = "2021"

[dependencies]
shore-llm-client = { workspace = true }
shore-config = { workspace = true }
rusqlite = { workspace = true }
reqwest = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
chrono = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
```

- [ ] **Step 2: Create stub module files**

`src/lib.rs`:
```rust
pub mod cache_tracker;
pub mod client;
pub mod ledger;
pub mod pricing;
pub mod query;
pub mod stream;

pub use client::{CallType, LedgerClient};
pub use ledger::Ledger;
pub use stream::LedgerStream;
```

`src/ledger.rs`:
```rust
//! SQLite-backed append-only ledger for LLM call recording.
```

`src/cache_tracker.rs`:
```rust
//! Per-character Anthropic cache warm/cold state machine.
```

`src/pricing.rs`:
```rust
//! Model pricing via OpenRouter API with local DB cache.
```

`src/client.rs`:
```rust
//! LedgerClient: compiler-enforced wrapper around LlmClient.
```

`src/stream.rs`:
```rust
//! LedgerStream: stream wrapper that records on finalization.
```

`src/query.rs`:
```rust
//! Aggregation and filter queries for the CLI.
```

- [ ] **Step 3: Add to workspace**

In the workspace root `Cargo.toml`:
- Add `"shore-ledger"` to the `members` array
- Add `shore-ledger = { path = "shore-ledger" }` to `[workspace.dependencies]`
- Promote `rusqlite = { version = "0.37", features = ["bundled"] }` to `[workspace.dependencies]` if not already there

Update `shore-daemon/Cargo.toml` to use `rusqlite = { workspace = true }` instead of its local version.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p shore-ledger`
Expected: Compiles with no errors (empty modules).

- [ ] **Step 5: Commit**

```bash
git add shore-ledger/ Cargo.toml shore-daemon/Cargo.toml
git commit -m "feat(ledger): scaffold shore-ledger crate with empty modules"
```

---

### Task 2: SQLite schema and Ledger struct

**Files:**
- Modify: `shore-ledger/src/ledger.rs`

- [ ] **Step 1: Write tests for Ledger creation and insert**

Add to `shore-ledger/src/ledger.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn test_ledger() -> Ledger {
        Ledger::open_in_memory().unwrap()
    }

    fn sample_row() -> CallRow {
        CallRow {
            ts: "2026-04-05T12:00:00Z".into(),
            character: "aria".into(),
            provider: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            call_type: "message".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 80,
            cache_write_tokens: 20,
            total_ms: 1500,
            ttft_ms: 200,
            finish_reason: "end_turn".into(),
            thinking_enabled: true,
            cache_state: Some("warm".into()),
            cache_anomaly: None,
            input_cost: Some(0.0015),
            output_cost: Some(0.00075),
            cache_read_cost: Some(0.0004),
            cache_write_cost: Some(0.0005),
            total_cost: Some(0.00315),
        }
    }

    #[test]
    fn create_and_insert() {
        let ledger = test_ledger();
        let row = sample_row();
        let id = ledger.insert(&row).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn insert_and_read_back() {
        let ledger = test_ledger();
        let row = sample_row();
        ledger.insert(&row).unwrap();

        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].character, "aria");
        assert_eq!(rows[0].input_tokens, 100);
        assert_eq!(rows[0].cache_read_tokens, 80);
        assert!(rows[0].cache_anomaly.is_none());
    }

    #[test]
    fn last_anthropic_call() {
        let ledger = test_ledger();
        let mut row = sample_row();
        row.cache_read_tokens = 80;
        ledger.insert(&row).unwrap();

        row.ts = "2026-04-05T12:01:00Z".into();
        row.cache_read_tokens = 120;
        ledger.insert(&row).unwrap();

        // Compaction call should be excluded
        row.ts = "2026-04-05T12:02:00Z".into();
        row.call_type = "compaction".into();
        row.cache_read_tokens = 0;
        ledger.insert(&row).unwrap();

        let last = ledger.last_anthropic_call("aria").unwrap().unwrap();
        assert_eq!(last.cache_read_tokens, 120);
    }

    #[test]
    fn null_costs_when_pricing_unavailable() {
        let ledger = test_ledger();
        let mut row = sample_row();
        row.input_cost = None;
        row.output_cost = None;
        row.cache_read_cost = None;
        row.cache_write_cost = None;
        row.total_cost = None;
        let id = ledger.insert(&row).unwrap();
        assert_eq!(id, 1);

        let rows = ledger.recent(1).unwrap();
        assert!(rows[0].total_cost.is_none());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p shore-ledger`
Expected: FAIL — `Ledger`, `CallRow`, methods not defined.

- [ ] **Step 3: Implement Ledger struct, CallRow, and schema**

In `shore-ledger/src/ledger.rs`:

```rust
use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, Result as SqlResult};

/// A single row in the calls table.
#[derive(Debug, Clone)]
pub struct CallRow {
    pub ts: String,
    pub character: String,
    pub provider: String,
    pub model: String,
    pub call_type: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub total_ms: u32,
    pub ttft_ms: u32,
    pub finish_reason: String,
    pub thinking_enabled: bool,
    pub cache_state: Option<String>,
    pub cache_anomaly: Option<String>,
    pub input_cost: Option<f64>,
    pub output_cost: Option<f64>,
    pub cache_read_cost: Option<f64>,
    pub cache_write_cost: Option<f64>,
    pub total_cost: Option<f64>,
}

pub struct Ledger {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS calls (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                  TEXT NOT NULL,
    character           TEXT NOT NULL,
    provider            TEXT NOT NULL,
    model               TEXT NOT NULL,
    call_type           TEXT NOT NULL,
    input_tokens        INTEGER NOT NULL,
    output_tokens       INTEGER NOT NULL,
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0,
    total_ms            INTEGER NOT NULL,
    ttft_ms             INTEGER NOT NULL DEFAULT 0,
    finish_reason       TEXT NOT NULL,
    thinking_enabled    INTEGER NOT NULL DEFAULT 0,
    cache_state         TEXT,
    cache_anomaly       TEXT,
    input_cost          REAL,
    output_cost         REAL,
    cache_read_cost     REAL,
    cache_write_cost    REAL,
    total_cost          REAL
);

CREATE INDEX IF NOT EXISTS idx_calls_ts ON calls(ts);
CREATE INDEX IF NOT EXISTS idx_calls_character ON calls(character);
CREATE INDEX IF NOT EXISTS idx_calls_provider ON calls(provider);
CREATE INDEX IF NOT EXISTS idx_calls_anomaly ON calls(cache_anomaly) WHERE cache_anomaly IS NOT NULL;

CREATE TABLE IF NOT EXISTS pricing (
    model_id              TEXT PRIMARY KEY,
    input_per_token       REAL NOT NULL,
    output_per_token      REAL NOT NULL,
    cache_read_per_token  REAL NOT NULL DEFAULT 0,
    cache_write_per_token REAL NOT NULL DEFAULT 0,
    fetched_at            TEXT NOT NULL
);
";

impl Ledger {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    pub fn insert(&self, row: &CallRow) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO calls (
                ts, character, provider, model, call_type,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                total_ms, ttft_ms, finish_reason, thinking_enabled,
                cache_state, cache_anomaly,
                input_cost, output_cost, cache_read_cost, cache_write_cost, total_cost
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10, ?11, ?12, ?13,
                ?14, ?15,
                ?16, ?17, ?18, ?19, ?20
            )",
            params![
                row.ts, row.character, row.provider, row.model, row.call_type,
                row.input_tokens, row.output_tokens, row.cache_read_tokens, row.cache_write_tokens,
                row.total_ms, row.ttft_ms, row.finish_reason, row.thinking_enabled as i32,
                row.cache_state, row.cache_anomaly,
                row.input_cost, row.output_cost, row.cache_read_cost, row.cache_write_cost, row.total_cost,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Return the N most recent rows, newest first.
    pub fn recent(&self, limit: u32) -> Result<Vec<CallRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT ts, character, provider, model, call_type,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    total_ms, ttft_ms, finish_reason, thinking_enabled,
                    cache_state, cache_anomaly,
                    input_cost, output_cost, cache_read_cost, cache_write_cost, total_cost
             FROM calls ORDER BY id DESC LIMIT ?1"
        )?;
        let rows = stmt.query_map(params![limit], row_from_sqlite)?.collect::<SqlResult<Vec<_>>>()?;
        Ok(rows)
    }

    /// Last non-compaction Anthropic call for a character.
    pub fn last_anthropic_call(&self, character: &str) -> Result<Option<CallRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT ts, character, provider, model, call_type,
                    input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    total_ms, ttft_ms, finish_reason, thinking_enabled,
                    cache_state, cache_anomaly,
                    input_cost, output_cost, cache_read_cost, cache_write_cost, total_cost
             FROM calls
             WHERE character = ?1 AND provider = 'anthropic' AND call_type != 'compaction'
             ORDER BY id DESC LIMIT 1"
        )?;
        let mut rows = stmt.query_map(params![character], row_from_sqlite)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }
}

fn row_from_sqlite(row: &rusqlite::Row) -> SqlResult<CallRow> {
    let thinking_int: i32 = row.get(12)?;
    Ok(CallRow {
        ts: row.get(0)?,
        character: row.get(1)?,
        provider: row.get(2)?,
        model: row.get(3)?,
        call_type: row.get(4)?,
        input_tokens: row.get(5)?,
        output_tokens: row.get(6)?,
        cache_read_tokens: row.get(7)?,
        cache_write_tokens: row.get(8)?,
        total_ms: row.get(9)?,
        ttft_ms: row.get(10)?,
        finish_reason: row.get(11)?,
        thinking_enabled: thinking_int != 0,
        cache_state: row.get(13)?,
        cache_anomaly: row.get(14)?,
        input_cost: row.get(15)?,
        output_cost: row.get(16)?,
        cache_read_cost: row.get(17)?,
        cache_write_cost: row.get(18)?,
        total_cost: row.get(19)?,
    })
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p shore-ledger`
Expected: All 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add shore-ledger/src/ledger.rs
git commit -m "feat(ledger): implement SQLite schema, Ledger struct with insert/query"
```

---

### Task 3: CacheTracker state machine

**Files:**
- Modify: `shore-ledger/src/cache_tracker.rs`

- [ ] **Step 1: Write tests for cache state transitions**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_cold() {
        let tracker = CacheTracker::new();
        assert_eq!(tracker.state(), CacheState::Cold);
    }

    #[test]
    fn cold_to_warm_on_cache_write() {
        let mut tracker = CacheTracker::new();
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn warm_stays_warm_on_increasing_cache_read() {
        let mut tracker = CacheTracker::new();
        // First call: cold → warm
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        // Second call: warm, cache read increased
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:30Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 500,
            cache_write_tokens: 50,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn warm_anomaly_on_cache_read_decrease() {
        let mut tracker = CacheTracker::new();
        // cold → warm
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        // warm, increasing read
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:30Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 500,
            cache_write_tokens: 50,
            call_type: "message".into(),
        });
        // warm, cache read DECREASED → anomaly
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:01:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 200,
            cache_write_tokens: 400,
            call_type: "message".into(),
        });
        assert_eq!(result.anomaly, Some(Anomaly::UnexpectedWrite));
    }

    #[test]
    fn cold_anomaly_on_unexpected_cache_read() {
        let mut tracker = CacheTracker::new();
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 500,
            cache_write_tokens: 0,
            call_type: "message".into(),
        });
        assert_eq!(result.anomaly, Some(Anomaly::UnexpectedRead));
    }

    #[test]
    fn compaction_transitions_to_cold() {
        let mut tracker = CacheTracker::new();
        // cold → warm
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);

        // compaction → cold (not anomaly)
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:01:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 300,
            call_type: "compaction".into(),
        });
        assert_eq!(tracker.state(), CacheState::Cold);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn model_change_transitions_to_cold() {
        let mut tracker = CacheTracker::new();
        // cold → warm with opus
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        // switch to sonnet → cold
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:01:00Z".into(),
            model: "claude-sonnet-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        // Model changed, so we go cold first, then cold→warm on write. No anomaly.
        assert_eq!(tracker.state(), CacheState::Warm);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn thinking_toggle_transitions_to_cold() {
        let mut tracker = CacheTracker::new();
        // cold → warm with thinking=true
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        // thinking toggled off → cold first, then evaluate
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:01:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: false,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm);
        assert!(result.anomaly.is_none());
    }

    #[test]
    fn ttl_expiry_transitions_to_cold() {
        let mut tracker = CacheTracker::with_ttl_secs(60); // 60s TTL
        // cold → warm
        tracker.observe(&Observation {
            ts: "2026-04-05T12:00:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        // 2 minutes later → TTL expired → cold
        let result = tracker.observe(&Observation {
            ts: "2026-04-05T12:02:00Z".into(),
            model: "claude-opus-4-6".into(),
            thinking_enabled: true,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            call_type: "message".into(),
        });
        assert_eq!(tracker.state(), CacheState::Warm); // cold then write → warm
        assert!(result.anomaly.is_none()); // expected after TTL expiry
    }

    #[test]
    fn reconstruct_warm_from_row() {
        let tracker = CacheTracker::reconstruct(
            "2026-04-05T11:59:30Z",
            "claude-opus-4-6",
            true,
            500, // cache_read > 0
            3600, // TTL 1h
        );
        // 30s ago, cache_read > 0 → should be warm
        // (this test assumes "now" is close to 2026-04-05T12:00:00Z — 
        //  in practice we'd need to mock time or use a relative check.
        //  The real implementation compares against current time.)
        assert_eq!(tracker.state(), CacheState::Warm);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p shore-ledger`
Expected: FAIL — `CacheTracker`, `CacheState`, `Observation`, `Anomaly`, etc. not defined.

- [ ] **Step 3: Implement CacheTracker**

```rust
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    Cold,
    Warm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anomaly {
    /// Cache was cold but we got a cache read — our cold-detection logic is wrong.
    UnexpectedRead,
    /// Cache was warm but cache_read decreased — prefix was invalidated.
    UnexpectedWrite,
}

/// Input data for a single cache observation.
pub struct Observation {
    pub ts: String,           // ISO 8601 UTC
    pub model: String,
    pub thinking_enabled: bool,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub call_type: String,
}

/// Result of processing an observation.
pub struct ObservationResult {
    pub state: CacheState,
    pub anomaly: Option<Anomaly>,
}

pub struct CacheTracker {
    state: CacheState,
    last_ts: Option<DateTime<Utc>>,
    last_model: Option<String>,
    last_thinking: Option<bool>,
    last_cache_read: u32,
    ttl_secs: u64,
}

impl CacheTracker {
    pub fn new() -> Self {
        Self::with_ttl_secs(3600) // default 1h
    }

    pub fn with_ttl_secs(ttl: u64) -> Self {
        Self {
            state: CacheState::Cold,
            last_ts: None,
            last_model: None,
            last_thinking: None,
            last_cache_read: 0,
            ttl_secs: ttl,
        }
    }

    /// Reconstruct tracker state from the last DB row.
    pub fn reconstruct(
        last_ts_str: &str,
        last_model: &str,
        last_thinking: bool,
        last_cache_read: u32,
        ttl_secs: u64,
    ) -> Self {
        let last_ts = DateTime::parse_from_rfc3339(last_ts_str)
            .ok()
            .map(|dt| dt.with_timezone(&Utc));

        let is_warm = last_ts
            .map(|ts| {
                let elapsed = Utc::now().signed_duration_since(ts).num_seconds();
                elapsed < ttl_secs as i64 && last_cache_read > 0
            })
            .unwrap_or(false);

        Self {
            state: if is_warm { CacheState::Warm } else { CacheState::Cold },
            last_ts,
            last_model: Some(last_model.to_string()),
            last_thinking: Some(last_thinking),
            last_cache_read,
            ttl_secs,
        }
    }

    pub fn state(&self) -> CacheState {
        self.state
    }

    pub fn last_cache_read(&self) -> u32 {
        self.last_cache_read
    }

    /// Process a new API response observation and return the resulting state + any anomaly.
    pub fn observe(&mut self, obs: &Observation) -> ObservationResult {
        let obs_ts = DateTime::parse_from_rfc3339(&obs.ts)
            .ok()
            .map(|dt| dt.with_timezone(&Utc));

        // Step 1: Check for transitions to Cold before evaluating
        if obs.call_type == "compaction" {
            self.state = CacheState::Cold;
            self.update_last(obs, obs_ts);
            return ObservationResult { state: self.state, anomaly: None };
        }

        // TTL expiry
        if self.state == CacheState::Warm {
            if let (Some(last), Some(now)) = (self.last_ts, obs_ts) {
                let elapsed = now.signed_duration_since(last).num_seconds();
                if elapsed > self.ttl_secs as i64 {
                    self.state = CacheState::Cold;
                }
            }
        }

        // Model change
        if self.state == CacheState::Warm {
            if let Some(ref last_model) = self.last_model {
                if last_model != &obs.model {
                    self.state = CacheState::Cold;
                }
            }
        }

        // Thinking toggle
        if self.state == CacheState::Warm {
            if let Some(last_thinking) = self.last_thinking {
                if last_thinking != obs.thinking_enabled {
                    self.state = CacheState::Cold;
                }
            }
        }

        // Step 2: Evaluate against expected behavior
        let anomaly = match self.state {
            CacheState::Cold => {
                if obs.cache_read_tokens > 0 {
                    Some(Anomaly::UnexpectedRead)
                } else {
                    None
                }
            }
            CacheState::Warm => {
                if obs.cache_read_tokens < self.last_cache_read {
                    Some(Anomaly::UnexpectedWrite)
                } else {
                    None
                }
            }
        };

        // Step 3: State transition
        if self.state == CacheState::Cold && obs.cache_write_tokens > 0 && obs.cache_read_tokens == 0 {
            self.state = CacheState::Warm;
        }

        self.update_last(obs, obs_ts);
        ObservationResult { state: self.state, anomaly }
    }

    fn update_last(&mut self, obs: &Observation, ts: Option<DateTime<Utc>>) {
        self.last_ts = ts;
        self.last_model = Some(obs.model.clone());
        self.last_thinking = Some(obs.thinking_enabled);
        self.last_cache_read = obs.cache_read_tokens;
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p shore-ledger`
Expected: All cache tracker tests PASS (the `reconstruct_warm_from_row` test may need adjustment depending on wall-clock time — see note in test. During implementation, use a time-injectable approach or adjust the test to use a timestamp within TTL of `Utc::now()`.)

- [ ] **Step 5: Commit**

```bash
git add shore-ledger/src/cache_tracker.rs
git commit -m "feat(ledger): implement CacheTracker state machine with anomaly detection"
```

---

### Task 4: PricingEngine

**Files:**
- Modify: `shore-ledger/src/pricing.rs`

- [ ] **Step 1: Write tests for pricing lookup and cost calculation**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn test_engine() -> PricingEngine {
        let ledger = crate::ledger::Ledger::open_in_memory().unwrap();
        PricingEngine::new(Arc::new(ledger))
    }

    #[test]
    fn calculate_cost_with_known_pricing() {
        let engine = test_engine();
        engine.store_pricing("anthropic/claude-opus-4-6", &ModelPricing {
            input_per_token: 0.000015,
            output_per_token: 0.000075,
            cache_read_per_token: 0.0000015,
            cache_write_per_token: 0.00001875,
        }).unwrap();

        let cost = engine.calculate_cost(
            "anthropic", "claude-opus-4-6",
            100, 50, 80, 20,
        ).unwrap();

        assert!(cost.is_some());
        let c = cost.unwrap();
        // input: 100 * 0.000015 = 0.0015
        assert!((c.input - 0.0015).abs() < 1e-10);
        // output: 50 * 0.000075 = 0.00375
        assert!((c.output - 0.00375).abs() < 1e-10);
        // cache_read: 80 * 0.0000015 = 0.00012
        assert!((c.cache_read - 0.00012).abs() < 1e-10);
        // cache_write: 20 * 0.00001875 = 0.000375
        assert!((c.cache_write - 0.000375).abs() < 1e-10);
    }

    #[test]
    fn returns_none_for_unknown_model() {
        let engine = test_engine();
        let cost = engine.calculate_cost(
            "unknown", "model", 100, 50, 0, 0,
        ).unwrap();
        assert!(cost.is_none());
    }

    #[test]
    fn model_id_mapping() {
        assert_eq!(to_openrouter_id("anthropic", "claude-opus-4-6"), "anthropic/claude-opus-4-6");
        assert_eq!(to_openrouter_id("openai", "gpt-4o"), "openai/gpt-4o");
    }

    #[test]
    fn store_and_retrieve_pricing() {
        let engine = test_engine();
        engine.store_pricing("anthropic/claude-opus-4-6", &ModelPricing {
            input_per_token: 0.000015,
            output_per_token: 0.000075,
            cache_read_per_token: 0.0000015,
            cache_write_per_token: 0.00001875,
        }).unwrap();

        let pricing = engine.get_cached_pricing("anthropic/claude-opus-4-6").unwrap();
        assert!(pricing.is_some());
        let p = pricing.unwrap();
        assert!((p.input_per_token - 0.000015).abs() < 1e-10);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p shore-ledger`
Expected: FAIL — `PricingEngine`, `ModelPricing`, `CostBreakdown`, `to_openrouter_id` not defined.

- [ ] **Step 3: Implement PricingEngine**

```rust
use std::sync::Arc;
use std::collections::HashMap;

use rusqlite::params;
use tracing::warn;

use crate::ledger::Ledger;

#[derive(Debug, Clone)]
pub struct ModelPricing {
    pub input_per_token: f64,
    pub output_per_token: f64,
    pub cache_read_per_token: f64,
    pub cache_write_per_token: f64,
}

#[derive(Debug, Clone)]
pub struct CostBreakdown {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

/// Maps our (provider, model) to OpenRouter's model ID format.
pub fn to_openrouter_id(provider: &str, model: &str) -> String {
    // Direct mapping works for most models.
    // Override table for known divergences.
    let key = format!("{provider}/{model}");
    OVERRIDES.get(key.as_str()).map(|s| s.to_string()).unwrap_or(key)
}

/// Hardcoded overrides for model IDs that don't match the simple pattern.
static OVERRIDES: std::sync::LazyLock<HashMap<&str, &str>> = std::sync::LazyLock::new(|| {
    HashMap::new() // Add overrides as discovered, e.g.:
    // map.insert("zai/some-model", "z-ai/some-model");
});

/// Anthropic 1h cache TTL price multipliers relative to the 5m TTL prices.
/// OpenRouter reports 5m prices. For 1h TTL:
/// - cache_write is 4x the 5m price
/// - cache_read is the same as 5m price
const ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER: f64 = 4.0;

pub struct PricingEngine {
    ledger: Arc<Ledger>,
    /// In-memory cache of fetched pricing (avoids DB reads on hot path).
    memory_cache: std::sync::Mutex<HashMap<String, ModelPricing>>,
}

impl PricingEngine {
    pub fn new(ledger: Arc<Ledger>) -> Self {
        Self {
            ledger,
            memory_cache: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Store pricing in both the DB and memory cache.
    pub fn store_pricing(&self, model_id: &str, pricing: &ModelPricing) -> Result<(), rusqlite::Error> {
        let conn = self.ledger.conn();
        conn.execute(
            "INSERT OR REPLACE INTO pricing (model_id, input_per_token, output_per_token, cache_read_per_token, cache_write_per_token, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                model_id,
                pricing.input_per_token,
                pricing.output_per_token,
                pricing.cache_read_per_token,
                pricing.cache_write_per_token,
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        self.memory_cache.lock().unwrap().insert(model_id.to_string(), pricing.clone());
        Ok(())
    }

    /// Get pricing from memory cache, then DB fallback.
    pub fn get_cached_pricing(&self, model_id: &str) -> Result<Option<ModelPricing>, rusqlite::Error> {
        // Check memory first
        if let Some(p) = self.memory_cache.lock().unwrap().get(model_id) {
            return Ok(Some(p.clone()));
        }
        // Check DB
        let conn = self.ledger.conn();
        let mut stmt = conn.prepare(
            "SELECT input_per_token, output_per_token, cache_read_per_token, cache_write_per_token
             FROM pricing WHERE model_id = ?1"
        )?;
        let result = stmt.query_row(params![model_id], |row| {
            Ok(ModelPricing {
                input_per_token: row.get(0)?,
                output_per_token: row.get(1)?,
                cache_read_per_token: row.get(2)?,
                cache_write_per_token: row.get(3)?,
            })
        });
        match result {
            Ok(p) => {
                self.memory_cache.lock().unwrap().insert(model_id.to_string(), p.clone());
                Ok(Some(p))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Fetch pricing for a model from OpenRouter. Store on success.
    pub async fn fetch_pricing(&self, provider: &str, model: &str) -> Result<Option<ModelPricing>, Box<dyn std::error::Error + Send + Sync>> {
        let model_id = to_openrouter_id(provider, model);

        let client = reqwest::Client::new();
        let url = format!("https://openrouter.ai/api/v1/models/{model_id}");
        let resp = client.get(&url).send().await?;

        if !resp.status().is_success() {
            warn!(model_id, status = %resp.status(), "failed to fetch pricing from OpenRouter");
            return Ok(None);
        }

        let body: serde_json::Value = resp.json().await?;
        let data = body.get("data").unwrap_or(&body);
        let pricing_obj = data.get("pricing");

        let pricing = match pricing_obj {
            Some(p) => {
                let input = parse_price(p.get("prompt"));
                let output = parse_price(p.get("completion"));
                let cache_read = parse_price(p.get("cache_read"));
                let mut cache_write = parse_price(p.get("cache_write"));

                // Apply 1h cache TTL multiplier for Anthropic
                if provider == "anthropic" {
                    cache_write *= ANTHROPIC_1H_CACHE_WRITE_MULTIPLIER;
                }

                ModelPricing {
                    input_per_token: input,
                    output_per_token: output,
                    cache_read_per_token: cache_read,
                    cache_write_per_token: cache_write,
                }
            }
            None => return Ok(None),
        };

        self.store_pricing(&model_id, &pricing)?;
        Ok(Some(pricing))
    }

    /// Get pricing for a model — tries memory, DB, then OpenRouter fetch.
    pub async fn get_or_fetch(&self, provider: &str, model: &str) -> Option<ModelPricing> {
        let model_id = to_openrouter_id(provider, model);

        // Try cached
        match self.get_cached_pricing(&model_id) {
            Ok(Some(p)) => return Some(p),
            Ok(None) => {}
            Err(e) => {
                warn!(error = %e, "failed to read pricing from DB");
            }
        }

        // Try fetch
        match self.fetch_pricing(provider, model).await {
            Ok(Some(p)) => Some(p),
            Ok(None) => {
                warn!(provider, model, "no pricing available from OpenRouter");
                None
            }
            Err(e) => {
                warn!(provider, model, error = %e, "failed to fetch pricing");
                None
            }
        }
    }

    /// Calculate cost for a single call. Returns None if pricing unavailable.
    pub fn calculate_cost(
        &self,
        provider: &str,
        model: &str,
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
    ) -> Result<Option<CostBreakdown>, rusqlite::Error> {
        let model_id = to_openrouter_id(provider, model);
        let pricing = match self.get_cached_pricing(&model_id)? {
            Some(p) => p,
            None => return Ok(None),
        };

        let input = input_tokens as f64 * pricing.input_per_token;
        let output = output_tokens as f64 * pricing.output_per_token;
        let cache_read = cache_read_tokens as f64 * pricing.cache_read_per_token;
        let cache_write = cache_write_tokens as f64 * pricing.cache_write_per_token;

        Ok(Some(CostBreakdown {
            input,
            output,
            cache_read,
            cache_write,
            total: input + output + cache_read + cache_write,
        }))
    }
}

fn parse_price(v: Option<&serde_json::Value>) -> f64 {
    v.and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| v.and_then(|v| v.as_f64()))
        .unwrap_or(0.0)
}
```

**Note:** The `store_pricing` method needs access to the raw SQLite connection. Add a `pub fn conn(&self) -> std::sync::MutexGuard<Connection>` method to `Ledger` so PricingEngine can write to the pricing table:

In `ledger.rs`, add:
```rust
impl Ledger {
    /// Expose the connection for sibling modules (pricing table writes).
    pub fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p shore-ledger`
Expected: All pricing tests PASS.

- [ ] **Step 5: Commit**

```bash
git add shore-ledger/src/pricing.rs shore-ledger/src/ledger.rs
git commit -m "feat(ledger): implement PricingEngine with OpenRouter fetch and DB cache"
```

---

### Task 5: LedgerClient wrapper (generate path)

**Files:**
- Modify: `shore-ledger/src/client.rs`
- Modify: `shore-ledger/src/lib.rs`

- [ ] **Step 1: Write tests for LedgerClient::generate recording**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;

    // We test the recording logic, not the LLM call itself.
    // Use record_generate directly with fabricated data.

    fn test_client_parts() -> (Arc<Ledger>, Arc<PricingEngine>, Arc<Mutex<HashMap<String, CacheTracker>>>) {
        let ledger = Arc::new(Ledger::open_in_memory().unwrap());
        let pricing = Arc::new(PricingEngine::new(ledger.clone()));
        let trackers = Arc::new(Mutex::new(HashMap::new()));
        (ledger, pricing, trackers)
    }

    #[test]
    fn record_generate_inserts_row() {
        let (ledger, pricing, trackers) = test_client_parts();
        let usage = shore_llm_client::types::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };
        let timing = shore_llm_client::types::Timing {
            total_ms: 1500,
            time_to_first_token_ms: 0,
        };

        record_call(
            &ledger, &pricing, &trackers,
            "anthropic", "claude-opus-4-6",
            CallType::Message, "aria",
            &usage, &timing,
            "end_turn", false,
        );

        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].character, "aria");
        assert_eq!(rows[0].call_type, "message");
        assert_eq!(rows[0].input_tokens, 100);
    }

    #[test]
    fn record_updates_cache_tracker() {
        let (ledger, pricing, trackers) = test_client_parts();
        let usage = shore_llm_client::types::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_creation_tokens: 500,
        };
        let timing = shore_llm_client::types::Timing {
            total_ms: 1500,
            time_to_first_token_ms: 0,
        };

        record_call(
            &ledger, &pricing, &trackers,
            "anthropic", "claude-opus-4-6",
            CallType::Message, "aria",
            &usage, &timing,
            "end_turn", true,
        );

        let tracker_map = trackers.lock().unwrap();
        let tracker = tracker_map.get("aria").unwrap();
        assert_eq!(tracker.state(), crate::cache_tracker::CacheState::Warm);
    }

    #[test]
    fn non_anthropic_skips_cache_tracker() {
        let (ledger, pricing, trackers) = test_client_parts();
        let usage = shore_llm_client::types::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        };
        let timing = shore_llm_client::types::Timing {
            total_ms: 500,
            time_to_first_token_ms: 0,
        };

        record_call(
            &ledger, &pricing, &trackers,
            "openai", "gpt-4o",
            CallType::Message, "aria",
            &usage, &timing,
            "stop", false,
        );

        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].cache_state.is_none());

        let tracker_map = trackers.lock().unwrap();
        assert!(!tracker_map.contains_key("aria"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p shore-ledger`
Expected: FAIL — `LedgerClient`, `CallType`, `record_call` not defined.

- [ ] **Step 3: Implement CallType and record_call**

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use tracing::error;

use crate::cache_tracker::{Anomaly, CacheTracker, Observation};
use crate::ledger::{CallRow, Ledger};
use crate::pricing::PricingEngine;
use shore_llm_client::types::{Usage, Timing};

#[derive(Debug, Clone, Copy)]
pub enum CallType {
    Message,
    ToolLoop,
    Keepalive,
    Interiority,
    Compaction,
    MemoryAgent,
    Researcher,
}

impl CallType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::ToolLoop => "tool_loop",
            Self::Keepalive => "keepalive",
            Self::Interiority => "interiority",
            Self::Compaction => "compaction",
            Self::MemoryAgent => "memory_agent",
            Self::Researcher => "researcher",
        }
    }
}

/// Core recording function used by both generate and stream paths.
pub(crate) fn record_call(
    ledger: &Ledger,
    pricing: &PricingEngine,
    cache_trackers: &Mutex<HashMap<String, CacheTracker>>,
    provider: &str,
    model: &str,
    call_type: CallType,
    character: &str,
    usage: &Usage,
    timing: &Timing,
    finish_reason: &str,
    thinking_enabled: bool,
) {
    let ts = Utc::now().to_rfc3339();

    // Cache tracking (Anthropic only)
    let (cache_state, cache_anomaly) = if provider == "anthropic" {
        let mut trackers = cache_trackers.lock().unwrap();
        let tracker = trackers.entry(character.to_string()).or_insert_with(CacheTracker::new);

        let result = tracker.observe(&Observation {
            ts: ts.clone(),
            model: model.to_string(),
            thinking_enabled,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_creation_tokens,
            call_type: call_type.as_str().to_string(),
        });

        let state_str = match result.state {
            crate::cache_tracker::CacheState::Cold => "cold",
            crate::cache_tracker::CacheState::Warm => "warm",
        };

        let anomaly_str = result.anomaly.map(|a| {
            let msg = match a {
                Anomaly::UnexpectedRead => "unexpected_read",
                Anomaly::UnexpectedWrite => "unexpected_write",
            };
            error!(
                character,
                provider,
                model,
                anomaly = msg,
                cache_read = usage.cache_read_tokens,
                cache_write = usage.cache_creation_tokens,
                "CACHE ANOMALY: {msg} — expected state was {state_str}"
            );
            msg.to_string()
        });

        (Some(state_str.to_string()), anomaly_str)
    } else {
        (None, None)
    };

    // Cost calculation (sync — uses cached pricing only, no fetch)
    let cost = pricing.calculate_cost(
        provider, model,
        usage.input_tokens, usage.output_tokens,
        usage.cache_read_tokens, usage.cache_creation_tokens,
    ).ok().flatten();

    let row = CallRow {
        ts,
        character: character.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        call_type: call_type.as_str().to_string(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: usage.cache_creation_tokens,
        total_ms: timing.total_ms,
        ttft_ms: timing.time_to_first_token_ms,
        finish_reason: finish_reason.to_string(),
        thinking_enabled,
        cache_state,
        cache_anomaly,
        input_cost: cost.as_ref().map(|c| c.input),
        output_cost: cost.as_ref().map(|c| c.output),
        cache_read_cost: cost.as_ref().map(|c| c.cache_read),
        cache_write_cost: cost.as_ref().map(|c| c.cache_write),
        total_cost: cost.as_ref().map(|c| c.total),
    };

    if let Err(e) = ledger.insert(&row) {
        error!(error = %e, "failed to insert ledger row — TOKEN DATA LOST");
    }
}
```

- [ ] **Step 4: Implement LedgerClient struct**

Still in `client.rs`, add the main wrapper struct:

```rust
use std::path::Path;

use shore_llm_client::{LlmClient, LlmError};
use shore_llm_client::types::{LlmRequest, GenerateResponse};
use shore_config::models::ResolvedModel;

use crate::stream::LedgerStream;

#[derive(Clone)]
pub struct LedgerClient {
    inner: LlmClient,
    ledger: Arc<Ledger>,
    cache_trackers: Arc<Mutex<HashMap<String, CacheTracker>>>,
    pricing: Arc<PricingEngine>,
}

impl LedgerClient {
    /// Construct a new LedgerClient, consuming the LlmClient.
    pub fn new(client: LlmClient, db_path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let ledger = Arc::new(Ledger::open(db_path)?);
        let pricing = Arc::new(PricingEngine::new(ledger.clone()));
        Ok(Self {
            inner: client,
            ledger,
            cache_trackers: Arc::new(Mutex::new(HashMap::new())),
            pricing,
        })
    }

    /// For tests: construct with in-memory DB.
    #[cfg(test)]
    pub fn new_in_memory(client: LlmClient) -> Self {
        let ledger = Arc::new(Ledger::open_in_memory().unwrap());
        let pricing = Arc::new(PricingEngine::new(ledger.clone()));
        Self {
            inner: client,
            ledger,
            cache_trackers: Arc::new(Mutex::new(HashMap::new())),
            pricing,
        }
    }

    /// Passthrough to LlmClient::build_request. No recording needed.
    pub fn build_request(
        model: &ResolvedModel,
        messages: Vec<serde_json::Value>,
        system: Option<serde_json::Value>,
        tools: Option<Vec<serde_json::Value>>,
        provider_options: Option<serde_json::Value>,
    ) -> Result<LlmRequest, LlmError> {
        LlmClient::build_request(model, messages, system, tools, provider_options)
    }

    /// Generate (non-streaming). Records to ledger automatically.
    pub async fn generate(
        &self,
        request: &LlmRequest,
        call_type: CallType,
        character: &str,
        thinking_enabled: bool,
    ) -> Result<GenerateResponse, LlmError> {
        // Ensure pricing is fetched for this model (lazy, async)
        self.pricing.get_or_fetch(&request.provider, &request.model).await;

        let resp = self.inner.generate(request, None).await?;

        record_call(
            &self.ledger, &self.pricing, &self.cache_trackers,
            &request.provider, &request.model,
            call_type, character,
            &resp.usage, &resp.timing,
            &resp.finish_reason, thinking_enabled,
        );

        Ok(resp)
    }

    /// Stream (returns LedgerStream that must be finalized).
    pub async fn stream_raw(
        &self,
        request: &LlmRequest,
        call_type: CallType,
        character: &str,
        thinking_enabled: bool,
    ) -> Result<LedgerStream, LlmError> {
        // Ensure pricing is fetched for this model (lazy, async)
        self.pricing.get_or_fetch(&request.provider, &request.model).await;

        let reader = self.inner.stream_raw(request, None).await?;

        Ok(LedgerStream::new(
            reader,
            request.provider.clone(),
            request.model.clone(),
            call_type,
            character.to_string(),
            thinking_enabled,
            self.ledger.clone(),
            self.pricing.clone(),
            self.cache_trackers.clone(),
        ))
    }

    /// Access the inner HTTP client for embed/image_generate passthrough.
    pub fn inner(&self) -> &LlmClient {
        &self.inner
    }

    /// Access the ledger for queries (used by CLI).
    pub fn ledger(&self) -> &Arc<Ledger> {
        &self.ledger
    }

    /// Access the pricing engine (used by CLI for refresh/recalculate).
    pub fn pricing(&self) -> &Arc<PricingEngine> {
        &self.pricing
    }

    /// Reconstruct cache tracker state from DB on startup.
    pub fn reconstruct_cache_state(&self, character: &str, ttl_secs: u64) {
        if let Ok(Some(row)) = self.ledger.last_anthropic_call(character) {
            let tracker = CacheTracker::reconstruct(
                &row.ts, &row.model, row.thinking_enabled,
                row.cache_read_tokens, ttl_secs,
            );
            self.cache_trackers.lock().unwrap().insert(character.to_string(), tracker);
        }
    }
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p shore-ledger`
Expected: All client tests PASS.

- [ ] **Step 6: Commit**

```bash
git add shore-ledger/src/client.rs shore-ledger/src/lib.rs
git commit -m "feat(ledger): implement LedgerClient wrapper with CallType and recording"
```

---

### Task 6: LedgerStream (streaming path)

**Files:**
- Modify: `shore-ledger/src/stream.rs`

- [ ] **Step 1: Write tests for LedgerStream finalization**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;
    use crate::pricing::PricingEngine;
    use crate::cache_tracker::CacheTracker;
    use shore_llm_client::types::{Usage, Timing, StreamResult};

    #[test]
    fn finalize_records_to_ledger() {
        let ledger = Arc::new(Ledger::open_in_memory().unwrap());
        let pricing = Arc::new(PricingEngine::new(ledger.clone()));
        let trackers = Arc::new(Mutex::new(HashMap::new()));

        let mut stream = LedgerStream::new_test(
            "anthropic".into(), "claude-opus-4-6".into(),
            crate::client::CallType::Message, "aria".into(),
            true,
            ledger.clone(), pricing, trackers,
        );

        let result = StreamResult {
            content: "Hello".into(),
            model: "claude-opus-4-6".into(),
            finish_reason: "end_turn".into(),
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: 80,
                cache_creation_tokens: 20,
            },
            timing: Timing { total_ms: 1500, time_to_first_token_ms: 200 },
            tool_uses: vec![],
            content_blocks: vec![],
        };

        stream.finalize(&result);
        assert!(stream.is_finalized());

        let rows = ledger.recent(1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 100);
        assert_eq!(rows[0].cache_read_tokens, 80);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p shore-ledger`
Expected: FAIL — `LedgerStream` not implemented.

- [ ] **Step 3: Implement LedgerStream**

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncRead, BufReader, DuplexStream};
use tracing::error;

use crate::cache_tracker::CacheTracker;
use crate::client::{record_call, CallType};
use crate::ledger::Ledger;
use crate::pricing::PricingEngine;
use shore_llm_client::types::StreamResult;

/// Wraps the stream reader and records to the ledger on finalization.
pub struct LedgerStream {
    reader: BufReader<DuplexStream>,
    provider: String,
    model: String,
    call_type: CallType,
    character: String,
    thinking_enabled: bool,
    ledger: Arc<Ledger>,
    pricing: Arc<PricingEngine>,
    cache_trackers: Arc<Mutex<HashMap<String, CacheTracker>>>,
    finalized: bool,
}

impl LedgerStream {
    pub(crate) fn new(
        reader: BufReader<DuplexStream>,
        provider: String,
        model: String,
        call_type: CallType,
        character: String,
        thinking_enabled: bool,
        ledger: Arc<Ledger>,
        pricing: Arc<PricingEngine>,
        cache_trackers: Arc<Mutex<HashMap<String, CacheTracker>>>,
    ) -> Self {
        Self {
            reader, provider, model, call_type, character,
            thinking_enabled, ledger, pricing, cache_trackers,
            finalized: false,
        }
    }

    /// For tests: create without a real reader.
    #[cfg(test)]
    pub(crate) fn new_test(
        provider: String,
        model: String,
        call_type: CallType,
        character: String,
        thinking_enabled: bool,
        ledger: Arc<Ledger>,
        pricing: Arc<PricingEngine>,
        cache_trackers: Arc<Mutex<HashMap<String, CacheTracker>>>,
    ) -> Self {
        let (_, read) = tokio::io::duplex(1);
        Self {
            reader: BufReader::new(read),
            provider, model, call_type, character,
            thinking_enabled, ledger, pricing, cache_trackers,
            finalized: false,
        }
    }

    /// Get mutable access to the underlying reader for StreamConsumer::consume().
    pub fn reader_mut(&mut self) -> &mut BufReader<DuplexStream> {
        &mut self.reader
    }

    /// Record the stream result to the ledger. Must be called after consume().
    pub fn finalize(&mut self, result: &StreamResult) {
        record_call(
            &self.ledger, &self.pricing, &self.cache_trackers,
            &self.provider, &self.model,
            self.call_type, &self.character,
            &result.usage, &result.timing,
            &result.finish_reason, self.thinking_enabled,
        );
        self.finalized = true;
    }

    pub fn is_finalized(&self) -> bool {
        self.finalized
    }
}

impl Drop for LedgerStream {
    fn drop(&mut self) {
        if !self.finalized {
            error!(
                provider = %self.provider,
                model = %self.model,
                character = %self.character,
                call_type = self.call_type.as_str(),
                "LedgerStream dropped without finalize — API call NOT recorded in ledger"
            );
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p shore-ledger`
Expected: All stream tests PASS.

- [ ] **Step 5: Commit**

```bash
git add shore-ledger/src/stream.rs
git commit -m "feat(ledger): implement LedgerStream with finalize-or-warn pattern"
```

---

### Task 7: Daemon integration — main.rs and handler.rs

**Files:**
- Modify: `shore-daemon/Cargo.toml`
- Modify: `shore-daemon/src/main.rs`
- Modify: `shore-daemon/src/handler.rs`

This task modifies the daemon to use `LedgerClient` instead of `LlmClient`. Start with the two highest-traffic call sites: main construction and the primary message handler.

- [ ] **Step 1: Add shore-ledger dependency to shore-daemon**

In `shore-daemon/Cargo.toml`, add:
```toml
shore-ledger = { workspace = true }
```

- [ ] **Step 2: Modify main.rs — construct LedgerClient**

In `shore-daemon/src/main.rs`, find the `LlmClient::new()` construction (around line 135). Replace with:

```rust
// Before:
let mut llm_client = LlmClient::new();

// After:
use shore_ledger::LedgerClient;

let raw_llm_client = {
    let mut c = LlmClient::new();
    if loaded.app.advanced.api_payload_logging {
        c.set_payload_log_dir(loaded.dirs.data.clone());
        info!("API payload logging enabled → {}/api_payloads.jsonl", loaded.dirs.data.display());
    }
    c
};
let ledger_db_path = loaded.dirs.data.join("ledger.db");
let llm_client = LedgerClient::new(raw_llm_client, &ledger_db_path)
    .expect("failed to initialize ledger database");
```

Update all places that reference the `llm_client` type. The `LedgerClient` is `Clone`, so `.clone()` calls still work. Everywhere that previously held `LlmClient` now holds `LedgerClient`.

After character initialization, reconstruct cache state from the ledger:
```rust
// After the character is loaded and cache_ttl is known:
let cache_ttl_secs = resolved.cache_ttl.as_deref()
    .and_then(parse_cache_ttl_secs)
    .unwrap_or(3600);
llm_client.reconstruct_cache_state(&char_name, cache_ttl_secs);
```

- [ ] **Step 3: Modify handler.rs — update stream_raw and generate calls**

The handler has the primary message flow. Find the `stream_raw` call (around line 845):

```rust
// Before:
let mut reader = ctx.llm_client.stream_raw(request, rid).await?;
let stream_result = consumer.consume(&mut reader, regen, &cache_ctx).await?;

// After:
let mut ledger_stream = ctx.llm_client.stream_raw(
    &request, CallType::Message, &char_name, thinking_enabled,
).await?;
let stream_result = consumer.consume(ledger_stream.reader_mut(), regen, &cache_ctx).await?;
ledger_stream.finalize(&stream_result);
```

Where `thinking_enabled` is derived from the resolved model config. Check `resolved.budget_tokens` or `resolved.reasoning_effort` to determine this — if either is set and non-zero, thinking is enabled.

Also update the `build_request` call (around line 657):
```rust
// Before:
let mut request = LlmClient::build_request(resolved, llm_messages, system, tool_defs, None)?;

// After:
let mut request = LedgerClient::build_request(resolved, llm_messages, system, tool_defs, None)?;
```

- [ ] **Step 4: Update imports throughout handler.rs**

Replace `use shore_llm_client::LlmClient` with `use shore_ledger::{LedgerClient, CallType}` wherever LlmClient is referenced.

The `MessageHandler` struct and `HandlerContext` struct (or equivalent) that stores the client need their type changed from `LlmClient` to `LedgerClient`.

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p shore-daemon`
Expected: May have errors in other files that still reference `LlmClient` methods with the old signature. Those are addressed in Task 8. For now, focus on handler.rs and main.rs compiling.

If other files cause errors, temporarily add `#[allow(unused)]` or comment out the broken calls with `todo!()` markers to isolate this task's changes.

- [ ] **Step 6: Commit**

```bash
git add shore-daemon/Cargo.toml shore-daemon/src/main.rs shore-daemon/src/handler.rs
git commit -m "feat(ledger): integrate LedgerClient into daemon main + handler"
```

---

### Task 8: Daemon integration — tools, autonomy, memory

**Files:**
- Modify: `shore-daemon/src/engine/tools.rs`
- Modify: `shore-daemon/src/autonomy/manager.rs`
- Modify: `shore-daemon/src/memory/agent_llm.rs`
- Modify: `shore-daemon/src/memory/compaction_impls.rs`
- Modify: `shore-daemon/src/memory/collation_impls.rs`

This task threads `LedgerClient`, `CallType`, and character name through all remaining LLM call sites.

- [ ] **Step 1: Update engine/tools.rs — tool loop**

Find `client.stream_raw(request, None)` (around line 234). The tool loop needs access to `CallType` and character name. Thread `character: &str` through the tool loop function parameters (from handler, which has `char_name`).

```rust
// Before:
let mut reader = client.stream_raw(request, None).await?;

// After:
let mut ledger_stream = client.stream_raw(
    &request, CallType::ToolLoop, character, thinking_enabled,
).await?;
let stream_result = consumer.consume(ledger_stream.reader_mut(), false, cache_ctx).await?;
ledger_stream.finalize(&stream_result);
```

Update the tool loop function signature to accept `character: &str` and `thinking_enabled: bool`. Thread these from the handler call site.

- [ ] **Step 2: Update autonomy/manager.rs — interiority and keepalive**

Find `client.generate(&request, None)` calls:

For interiority (around line 912):
```rust
// Before:
let resp = match client.generate(&request, None).await {

// After:
let resp = match client.generate(&request, CallType::Interiority, character, thinking_enabled).await {
```

For keepalive/dormant ping (around line 1248):
```rust
// Before:
match client.generate(&request, None).await {

// After:
match client.generate(&request, CallType::Keepalive, character, thinking_enabled).await {
```

Also update `LlmClient::build_request` → `LedgerClient::build_request` (around line 817).

- [ ] **Step 3: Update memory/agent_llm.rs — add character field**

The `RealAgentLlm` struct stores an `LlmClient`. Add a `character: String` field:

```rust
pub struct RealAgentLlm {
    client: LedgerClient,  // was LlmClient
    character: String,      // NEW
}
```

Update the constructor to accept `character: String`.

Update the `generate` method (around line 95):
```rust
// Before:
let resp = self.client.generate(&request, None).await

// After:
let resp = self.client.generate(&request, CallType::MemoryAgent, &self.character, false).await
```

And update `build_request` (around line 90):
```rust
// Before:
let request = LlmClient::build_request(model, messages, system, tools, None)

// After:
let request = LedgerClient::build_request(model, messages, system, tools, None)
```

- [ ] **Step 4: Update memory/compaction_impls.rs — add character field**

Same pattern as agent_llm. The `RealCompactionLlm` struct needs a `character: String` field:

```rust
pub struct RealCompactionLlm {
    client: LedgerClient,  // was LlmClient
    model: ResolvedModel,
    character: String,      // NEW
}
```

Update generate (around line 261):
```rust
let resp = self.client.generate(&request, CallType::Compaction, &self.character, false).await
```

- [ ] **Step 5: Update memory/collation_impls.rs — add character field**

Same pattern:

```rust
pub struct CollationLlm {
    client: LedgerClient,
    model: ResolvedModel,
    character: String,
}
```

Update generate call to pass `CallType::MemoryAgent` and `&self.character`.

- [ ] **Step 6: Thread character through constructors**

Find everywhere `RealAgentLlm`, `RealCompactionLlm`, and `CollationLlm` are constructed and pass the character name. These are typically constructed in:
- `handler.rs` or `main.rs` (where character name is available)
- The autonomy manager (where `character: &str` is a parameter)

- [ ] **Step 7: Add notification service integration for cache anomalies**

In the daemon, after each LLM call that goes through the ledger, check if an anomaly was recorded. The cleanest approach: add an `observe_and_notify` helper in the daemon that wraps the ledger's recording and fires notifications. Alternatively, `LedgerClient` can accept an optional notification callback at construction time.

Simplest approach — check the ledger's last row after finalization:
```rust
// After ledger_stream.finalize(&stream_result):
if let Ok(Some(row)) = llm_client.ledger().recent(1) {
    if let Some(ref anomaly) = row.first().and_then(|r| r.cache_anomaly.as_ref()) {
        notifier.notify(
            NotificationEvent::CacheWarning,
            "Cache Anomaly",
            &format!("{anomaly}: {} cache_read={} cache_write={}",
                row[0].model, row[0].cache_read_tokens, row[0].cache_write_tokens),
        );
    }
}
```

This needs to be added at each call site in the handler where the notifier is accessible. For background tasks (autonomy, memory), log the error via `tracing::error!` (already done in `record_call`), and fire the notification if the notifier is accessible.

- [ ] **Step 8: Verify full daemon compiles**

Run: `cargo check -p shore-daemon`
Expected: Compiles with no errors. All LlmClient references should now be LedgerClient.

- [ ] **Step 9: Verify no remaining raw LlmClient usage in daemon**

Run: `grep -rn "LlmClient" shore-daemon/src/`
Expected: Only `use` statements for types from shore-llm-client that are still needed (like `LlmError`, `LlmRequest`, etc.), but NO direct construction or method calls on `LlmClient`. The type `LlmClient` should only appear in shore-ledger's internal wrapping.

- [ ] **Step 10: Commit**

```bash
git add shore-daemon/src/
git commit -m "feat(ledger): thread LedgerClient + CallType through all daemon LLM call sites"
```

---

### Task 9: Query module for CLI

**Files:**
- Modify: `shore-ledger/src/query.rs`

- [ ] **Step 1: Write tests for query aggregation**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{CallRow, Ledger};

    fn populated_ledger() -> Ledger {
        let ledger = Ledger::open_in_memory().unwrap();
        let base = CallRow {
            ts: "2026-04-05T10:00:00Z".into(),
            character: "aria".into(),
            provider: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            call_type: "message".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 80,
            cache_write_tokens: 20,
            total_ms: 1500,
            ttft_ms: 200,
            finish_reason: "end_turn".into(),
            thinking_enabled: true,
            cache_state: Some("warm".into()),
            cache_anomaly: None,
            input_cost: Some(0.0015),
            output_cost: Some(0.00375),
            cache_read_cost: Some(0.00012),
            cache_write_cost: Some(0.000375),
            total_cost: Some(0.005745),
        };
        ledger.insert(&base).unwrap();

        let mut row2 = base.clone();
        row2.ts = "2026-04-05T10:01:00Z".into();
        row2.call_type = "tool_loop".into();
        row2.input_tokens = 200;
        row2.total_cost = Some(0.01);
        ledger.insert(&row2).unwrap();

        let mut row3 = base.clone();
        row3.ts = "2026-04-05T10:02:00Z".into();
        row3.provider = "openai".into();
        row3.model = "gpt-4o".into();
        row3.cache_read_tokens = 0;
        row3.cache_write_tokens = 0;
        row3.cache_state = None;
        row3.total_cost = Some(0.002);
        ledger.insert(&row3).unwrap();

        ledger
    }

    #[test]
    fn summary_groups_by_provider_model() {
        let ledger = populated_ledger();
        let summary = usage_summary(&ledger, &QueryFilter::default()).unwrap();
        assert_eq!(summary.len(), 2); // anthropic/opus + openai/gpt-4o
        let anthropic = summary.iter().find(|s| s.provider == "anthropic").unwrap();
        assert_eq!(anthropic.call_count, 2);
    }

    #[test]
    fn filter_by_provider() {
        let ledger = populated_ledger();
        let filter = QueryFilter { provider: Some("anthropic".into()), ..Default::default() };
        let summary = usage_summary(&ledger, &filter).unwrap();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].call_count, 2);
    }

    #[test]
    fn anomalies_query() {
        let ledger = Ledger::open_in_memory().unwrap();
        let mut row = CallRow {
            ts: "2026-04-05T10:00:00Z".into(),
            character: "aria".into(),
            provider: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            call_type: "message".into(),
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_write_tokens: 500,
            total_ms: 1500,
            ttft_ms: 200,
            finish_reason: "end_turn".into(),
            thinking_enabled: true,
            cache_state: Some("cold".into()),
            cache_anomaly: Some("unexpected_read".into()),
            input_cost: None, output_cost: None,
            cache_read_cost: None, cache_write_cost: None,
            total_cost: None,
        };
        ledger.insert(&row).unwrap();
        row.cache_anomaly = None;
        ledger.insert(&row).unwrap();

        let anomalies = query_anomalies(&ledger, &QueryFilter::default()).unwrap();
        assert_eq!(anomalies.len(), 1);
        assert_eq!(anomalies[0].cache_anomaly, Some("unexpected_read".into()));
    }

    #[test]
    fn export_csv_format() {
        let ledger = populated_ledger();
        let csv = export_csv(&ledger, &QueryFilter::default()).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[0].contains("ts\t")); // header
        assert_eq!(lines.len(), 4); // header + 3 rows
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p shore-ledger`
Expected: FAIL — query types and functions not defined.

- [ ] **Step 3: Implement query module**

```rust
use crate::ledger::{CallRow, Ledger};
use rusqlite::params;

#[derive(Debug, Default, Clone)]
pub struct QueryFilter {
    pub since: Option<String>,       // ISO 8601 timestamp
    pub until: Option<String>,
    pub character: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub call_type: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UsageSummary {
    pub provider: String,
    pub model: String,
    pub call_count: u32,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_read: u64,
    pub total_cache_write: u64,
    pub total_cost: Option<f64>,
}

/// Build a WHERE clause and params from a QueryFilter.
fn build_where(filter: &QueryFilter) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut conditions = Vec::new();
    let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(ref since) = filter.since {
        conditions.push(format!("ts >= ?{}", params_vec.len() + 1));
        params_vec.push(Box::new(since.clone()));
    }
    if let Some(ref until) = filter.until {
        conditions.push(format!("ts <= ?{}", params_vec.len() + 1));
        params_vec.push(Box::new(until.clone()));
    }
    if let Some(ref character) = filter.character {
        conditions.push(format!("character = ?{}", params_vec.len() + 1));
        params_vec.push(Box::new(character.clone()));
    }
    if let Some(ref provider) = filter.provider {
        conditions.push(format!("provider = ?{}", params_vec.len() + 1));
        params_vec.push(Box::new(provider.clone()));
    }
    if let Some(ref model) = filter.model {
        conditions.push(format!("model = ?{}", params_vec.len() + 1));
        params_vec.push(Box::new(model.clone()));
    }
    if let Some(ref call_type) = filter.call_type {
        conditions.push(format!("call_type = ?{}", params_vec.len() + 1));
        params_vec.push(Box::new(call_type.clone()));
    }

    let clause = if conditions.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conditions.join(" AND "))
    };

    (clause, params_vec)
}

pub fn usage_summary(ledger: &Ledger, filter: &QueryFilter) -> Result<Vec<UsageSummary>, rusqlite::Error> {
    let (where_clause, params_vec) = build_where(filter);
    let sql = format!(
        "SELECT provider, model, COUNT(*) as cnt,
                SUM(input_tokens), SUM(output_tokens),
                SUM(cache_read_tokens), SUM(cache_write_tokens),
                SUM(total_cost)
         FROM calls {where_clause}
         GROUP BY provider, model
         ORDER BY SUM(total_cost) DESC NULLS LAST"
    );

    let conn = ledger.conn();
    let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok(UsageSummary {
            provider: row.get(0)?,
            model: row.get(1)?,
            call_count: row.get(2)?,
            total_input: row.get(3)?,
            total_output: row.get(4)?,
            total_cache_read: row.get(5)?,
            total_cache_write: row.get(6)?,
            total_cost: row.get(7)?,
        })
    })?.collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn query_anomalies(ledger: &Ledger, filter: &QueryFilter) -> Result<Vec<CallRow>, rusqlite::Error> {
    let (mut where_clause, mut params_vec) = build_where(filter);
    if where_clause.is_empty() {
        where_clause = "WHERE cache_anomaly IS NOT NULL".into();
    } else {
        where_clause.push_str(" AND cache_anomaly IS NOT NULL");
    }

    let sql = format!(
        "SELECT ts, character, provider, model, call_type,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                total_ms, ttft_ms, finish_reason, thinking_enabled,
                cache_state, cache_anomaly,
                input_cost, output_cost, cache_read_cost, cache_write_cost, total_cost
         FROM calls {where_clause} ORDER BY id DESC"
    );

    let conn = ledger.conn();
    let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(param_refs.as_slice(), crate::ledger::row_from_sqlite)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn export_csv(ledger: &Ledger, filter: &QueryFilter) -> Result<String, rusqlite::Error> {
    let (where_clause, params_vec) = build_where(filter);
    let sql = format!(
        "SELECT ts, character, provider, model, call_type,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                total_ms, ttft_ms, finish_reason, thinking_enabled,
                cache_state, cache_anomaly,
                input_cost, output_cost, cache_read_cost, cache_write_cost, total_cost
         FROM calls {where_clause} ORDER BY id"
    );

    let conn = ledger.conn();
    let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
    let mut stmt = conn.prepare(&sql)?;

    let mut out = String::new();
    out.push_str("ts\tcharacter\tprovider\tmodel\tcall_type\tinput_tokens\toutput_tokens\tcache_read_tokens\tcache_write_tokens\ttotal_ms\tttft_ms\tfinish_reason\tthinking_enabled\tcache_state\tcache_anomaly\tinput_cost\toutput_cost\tcache_read_cost\tcache_write_cost\ttotal_cost\n");

    let rows = stmt.query_map(param_refs.as_slice(), crate::ledger::row_from_sqlite)?;
    for row in rows {
        let r = row?;
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.ts, r.character, r.provider, r.model, r.call_type,
            r.input_tokens, r.output_tokens, r.cache_read_tokens, r.cache_write_tokens,
            r.total_ms, r.ttft_ms, r.finish_reason, r.thinking_enabled as u8,
            r.cache_state.as_deref().unwrap_or(""),
            r.cache_anomaly.as_deref().unwrap_or(""),
            opt_f64(r.input_cost), opt_f64(r.output_cost),
            opt_f64(r.cache_read_cost), opt_f64(r.cache_write_cost),
            opt_f64(r.total_cost),
        ));
    }
    Ok(out)
}

fn opt_f64(v: Option<f64>) -> String {
    v.map(|f| format!("{f:.8}")).unwrap_or_default()
}

/// Return the last Anthropic call row per active character (for cache health display).
pub fn active_anthropic_characters(ledger: &Ledger, filter: &QueryFilter) -> Result<Vec<(String, CallRow)>, rusqlite::Error> {
    // Query distinct characters with recent Anthropic calls, then get last row for each.
    let (where_clause, params_vec) = build_where(filter);
    let extra = if where_clause.is_empty() {
        "WHERE provider = 'anthropic'"
    } else {
        // append: AND provider = 'anthropic'
        // (implementation detail — adjust the dynamic SQL accordingly)
        ""
    };
    // For each character, call ledger.last_anthropic_call(character)
    // Return vec of (character, last_row) pairs.
    todo!("implementation uses DISTINCT character query + last_anthropic_call per character")
}

/// Count consecutive warm calls for a character (for "streak: N calls" display).
pub fn warm_streak(ledger: &Ledger, character: &str) -> Result<u32, rusqlite::Error> {
    // Walk backwards from most recent call while cache_state = 'warm'
    let conn = ledger.conn();
    let mut stmt = conn.prepare(
        "SELECT cache_state FROM calls
         WHERE character = ?1 AND provider = 'anthropic' AND call_type != 'compaction'
         ORDER BY id DESC"
    )?;
    let mut count = 0u32;
    let rows = stmt.query_map(params![character], |row| row.get::<_, Option<String>>(0))?;
    for row in rows {
        if row?.as_deref() == Some("warm") {
            count += 1;
        } else {
            break;
        }
    }
    Ok(count)
}

/// Recalculate costs for rows with NULL total_cost using current pricing.
pub async fn recalculate_costs(ledger: &Arc<Ledger>, pricing: &PricingEngine) -> u32 {
    // Select rows where total_cost IS NULL, calculate cost, update.
    let conn = ledger.conn();
    let mut stmt = conn.prepare(
        "SELECT id, provider, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens
         FROM calls WHERE total_cost IS NULL"
    ).unwrap();
    let rows: Vec<(i64, String, String, u32, u32, u32, u32)> = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
    }).unwrap().filter_map(|r| r.ok()).collect();
    drop(stmt);
    drop(conn);

    let mut updated = 0u32;
    for (id, provider, model, input, output, cache_read, cache_write) in &rows {
        // Ensure pricing is fetched
        pricing.get_or_fetch(provider, model).await;
        if let Ok(Some(cost)) = pricing.calculate_cost(provider, model, *input, *output, *cache_read, *cache_write) {
            let conn = ledger.conn();
            let _ = conn.execute(
                "UPDATE calls SET input_cost=?1, output_cost=?2, cache_read_cost=?3, cache_write_cost=?4, total_cost=?5 WHERE id=?6",
                params![cost.input, cost.output, cost.cache_read, cost.cache_write, cost.total, id],
            );
            updated += 1;
        }
    }
    updated
}
```

Also add a `clear_cache` method to `PricingEngine` in Task 4:

```rust
impl PricingEngine {
    pub fn clear_cache(&self) -> Result<(), rusqlite::Error> {
        let conn = self.ledger.conn();
        conn.execute("DELETE FROM pricing", [])?;
        self.memory_cache.lock().unwrap().clear();
        Ok(())
    }
}
```

**Note:** The `row_from_sqlite` function in `ledger.rs` needs to be made `pub(crate)` so `query.rs` can use it. Also rename `export_csv` to `export_delimited` and accept a delimiter parameter (or provide both `export_tsv` and `export_csv` functions). The default export uses tabs (TSV); CSV mode replaces with commas.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p shore-ledger`
Expected: All query tests PASS.

- [ ] **Step 5: Commit**

```bash
git add shore-ledger/src/query.rs shore-ledger/src/ledger.rs
git commit -m "feat(ledger): implement query module with aggregation, filtering, and TSV export"
```

---

### Task 10: CLI usage subcommand

**Files:**
- Modify: `shore-cli/Cargo.toml`
- Modify: `shore-cli/src/cli.rs`
- Modify: `shore-cli/src/run.rs` (or `main.rs`, depending on dispatch location)

The `shore usage` command reads the ledger DB directly — no daemon connection needed.

- [ ] **Step 1: Add shore-ledger and shore-config dependencies to shore-cli**

In `shore-cli/Cargo.toml`, add:
```toml
shore-ledger = { workspace = true }
```

(`shore-config` should already be a dependency.)

- [ ] **Step 2: Add Usage variant to CliCommand enum**

In `shore-cli/src/cli.rs`, add to the `CliCommand` enum:

```rust
/// Show token usage statistics and costs
Usage {
    /// Time period: "today", "7d", "30d", "all" (default: today)
    #[arg(long, default_value = "today")]
    last: String,

    /// Filter by character name
    #[arg(long)]
    character: Option<String>,

    /// Filter by provider
    #[arg(long)]
    provider: Option<String>,

    /// Filter by model
    #[arg(long)]
    model: Option<String>,

    /// Filter by call type
    #[arg(long)]
    call_type: Option<String>,

    /// Show only cache anomalies
    #[arg(long)]
    anomalies: bool,

    /// Export full ledger as CSV to stdout
    #[arg(long)]
    export_csv: bool,

    /// Export full ledger as TSV to stdout
    #[arg(long)]
    export_tsv: bool,

    /// Clear cached pricing data
    #[arg(long)]
    refresh_pricing: bool,

    /// Recalculate costs using current pricing
    #[arg(long)]
    recalculate: bool,
},
```

- [ ] **Step 3: Handle Usage in the CLI dispatch (local, no daemon)**

In `shore-cli/src/run.rs` (or wherever local commands are handled before daemon connection), add a handler for `Usage`. This runs before the SWP connection is established:

```rust
CliCommand::Usage {
    last, character, provider, model, call_type,
    anomalies, export_csv, export_tsv,
    refresh_pricing, recalculate,
} => {
    let data_dir = shore_config::data_dir();
    let db_path = data_dir.join("ledger.db");

    if !db_path.exists() {
        eprintln!("No ledger found at {}. Run the daemon first to start recording.", db_path.display());
        std::process::exit(1);
    }

    let ledger = shore_ledger::Ledger::open(&db_path)
        .expect("failed to open ledger database");

    let since = parse_last_period(&last);
    let filter = shore_ledger::query::QueryFilter {
        since,
        character: character.clone(),
        provider: provider.clone(),
        model: model.clone(),
        call_type: call_type.clone(),
        ..Default::default()
    };

    if *export_csv || *export_tsv {
        // TSV is the default export format (export_csv uses commas)
        let output = shore_ledger::query::export_csv(&ledger, &filter).unwrap();
        if *export_csv {
            // Replace tabs with commas for CSV
            print!("{}", output.replace('\t', ","));
        } else {
            print!("{}", output);
        }
        return Ok(());
    }

    if *anomalies {
        let rows = shore_ledger::query::query_anomalies(&ledger, &filter).unwrap();
        if rows.is_empty() {
            println!("No cache anomalies found.");
        } else {
            println!("Cache Anomalies:\n");
            for r in &rows {
                println!(
                    "  {} {} {} {} — {} (read: {}, write: {})",
                    r.ts, r.character, r.model, r.call_type,
                    r.cache_anomaly.as_deref().unwrap_or("?"),
                    r.cache_read_tokens, r.cache_write_tokens,
                );
            }
            println!("\nTotal: {} anomalies", rows.len());
        }
        return Ok(());
    }

    if *refresh_pricing {
        let pricing = shore_ledger::pricing::PricingEngine::new(std::sync::Arc::new(ledger));
        pricing.clear_cache().unwrap();
        println!("Pricing cache cleared. Prices will be re-fetched on next daemon use.");
        return Ok(());
    }

    if *recalculate {
        let ledger = std::sync::Arc::new(ledger);
        let pricing = shore_ledger::pricing::PricingEngine::new(ledger.clone());
        let rt = tokio::runtime::Runtime::new().unwrap();
        let updated = rt.block_on(shore_ledger::query::recalculate_costs(&ledger, &pricing));
        println!("Recalculated costs for {updated} rows.");
        return Ok(());
    }

    // Default: usage summary + cache health
    let summary = shore_ledger::query::usage_summary(&ledger, &filter).unwrap();
    print_usage_summary(&summary, &last);

    // Cache health: reconstruct tracker state from DB for each character
    let characters = shore_ledger::query::active_anthropic_characters(&ledger, &filter).unwrap();
    if !characters.is_empty() {
        println!("\nCache Health (anthropic):");
        for (character, last_row) in &characters {
            let tracker = shore_ledger::cache_tracker::CacheTracker::reconstruct(
                &last_row.ts, &last_row.model, last_row.thinking_enabled,
                last_row.cache_read_tokens, 3600,
            );
            let state_str = match tracker.state() {
                shore_ledger::cache_tracker::CacheState::Warm => {
                    let streak = shore_ledger::query::warm_streak(&ledger, character).unwrap_or(0);
                    format!("Warm (streak: {} calls)", streak)
                }
                shore_ledger::cache_tracker::CacheState::Cold => {
                    "Cold".into()
                }
            };
            println!("  {character:<8} — {state_str}");
        }
    }

    return Ok(());
}
```

- [ ] **Step 4: Implement helper functions**

```rust
fn parse_last_period(period: &str) -> Option<String> {
    let now = chrono::Utc::now();
    let since = match period {
        "today" => now.date_naive().and_hms_opt(0, 0, 0)
            .map(|dt| dt.and_utc()),
        "all" => None,
        s => {
            // Parse "Nd" format
            if let Some(days_str) = s.strip_suffix('d') {
                if let Ok(days) = days_str.parse::<i64>() {
                    Some(now - chrono::Duration::days(days))
                } else {
                    None
                }
            } else {
                None
            }
        }
    };
    since.flatten().or(since).map(|dt| dt.to_rfc3339())
    // (Adjust the flatten/map chain based on actual chrono API —
    //  the key point is converting "today"/"7d"/etc. to an ISO timestamp)
}

fn print_usage_summary(summary: &[shore_ledger::query::UsageSummary], period: &str) {
    let today = chrono::Utc::now().format("%Y-%m-%d");
    println!("Shore Usage — {today} (period: {period})\n");
    println!(
        "{:<12} {:<24} {:>5}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}",
        "Provider", "Model", "Calls", "Input", "Output", "Cache R", "Cache W", "Cost"
    );
    println!("{}", "-".repeat(95));

    let mut grand_total = 0.0f64;
    for s in summary {
        let cost_str = s.total_cost
            .map(|c| { grand_total += c; format!("${c:.2}") })
            .unwrap_or_else(|| "—".into());
        println!(
            "{:<12} {:<24} {:>5}  {:>8}K  {:>8}K  {:>8}K  {:>8}K  {:>8}",
            s.provider, s.model, s.call_count,
            format_k(s.total_input), format_k(s.total_output),
            format_k(s.total_cache_read), format_k(s.total_cache_write),
            cost_str,
        );
    }
    println!("{:>87} ${grand_total:.2}", "Total:");
}

fn format_k(tokens: u64) -> String {
    if tokens == 0 {
        "—".into()
    } else if tokens < 1000 {
        tokens.to_string()
    } else {
        format!("{:.1}", tokens as f64 / 1000.0)
    }
}
```

- [ ] **Step 5: Verify CLI compiles**

Run: `cargo check -p shore-cli`
Expected: Compiles with no errors.

- [ ] **Step 6: Commit**

```bash
git add shore-cli/
git commit -m "feat(cli): add 'shore usage' subcommand for querying token costs and anomalies"
```

---

### Task 11: Full build verification and live test

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: All crates compile successfully.

- [ ] **Step 2: Run all unit tests**

Run: `cargo test --workspace`
Expected: All tests pass, including new shore-ledger tests.

- [ ] **Step 3: Live test with test character**

Build the daemon and run a real API call using the test character. Verify:
1. The daemon starts and creates `ledger.db` in the data directory
2. A message generates a response
3. `shore usage` shows the call with token counts and cost
4. The ledger.db contains the expected row

Run:
```bash
cargo build --workspace --release
# Start daemon with test character, send a message, then:
./target/release/shore usage
```

Expected: Usage summary shows at least one row with non-zero token counts and a dollar cost.

- [ ] **Step 4: Verify cache tracking**

Send multiple messages to the same character. After the first (cold → warm), subsequent calls should show `cache_state = warm` and increasing `cache_read_tokens`. Check:

```bash
./target/release/shore usage --export-tsv | head -20
```

Expected: First row has `cache_state=cold`, subsequent rows have `cache_state=warm` with `cache_read_tokens` monotonically increasing.

- [ ] **Step 5: Update documentation**

Record the new crate, its purpose, and the CLI command in:
- `DECISIONS.md` — decision to use SQLite, OpenRouter for pricing, compiler-enforced recording
- `ARCHITECTURE.md` — new shore-ledger crate, dependency graph change
- `QUIRKS.md` — Anthropic 1h cache TTL multiplier, any surprises discovered during implementation

- [ ] **Step 6: Final commit**

```bash
git add DECISIONS.md ARCHITECTURE.md QUIRKS.md
git commit -m "docs: record shore-ledger architecture decisions and quirks"
```
