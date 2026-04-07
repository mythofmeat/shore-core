//! SQLite-backed append-only ledger for LLM call recording.

use rusqlite::{params, Connection, Result as SqlResult};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tracing::{debug, info};

// ── Schema ────────────────────────────────────────────────────────────────────

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS calls (
    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                  TEXT    NOT NULL,
    character           TEXT    NOT NULL,
    provider            TEXT    NOT NULL,
    model               TEXT    NOT NULL,
    call_type           TEXT    NOT NULL,
    input_tokens        INTEGER NOT NULL,
    output_tokens       INTEGER NOT NULL,
    cache_read_tokens   INTEGER NOT NULL,
    cache_write_tokens  INTEGER NOT NULL,
    cache_ttl           TEXT    DEFAULT '1h',
    total_ms            INTEGER NOT NULL,
    ttft_ms             INTEGER NOT NULL,
    finish_reason       TEXT    NOT NULL,
    thinking_enabled    INTEGER NOT NULL,
    cache_state         TEXT,
    cache_anomaly       TEXT,
    input_cost          REAL,
    output_cost         REAL,
    cache_read_cost     REAL,
    cache_write_cost    REAL,
    total_cost          REAL
);

CREATE TABLE IF NOT EXISTS pricing (
    model_id                TEXT PRIMARY KEY,
    input_per_token         REAL NOT NULL,
    output_per_token        REAL NOT NULL,
    cache_read_per_token    REAL NOT NULL,
    cache_write_per_token   REAL NOT NULL,
    cache_write_1h_per_token REAL NOT NULL,
    fetched_at              TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_calls_ts        ON calls (ts);
CREATE INDEX IF NOT EXISTS idx_calls_character ON calls (character);
CREATE INDEX IF NOT EXISTS idx_calls_provider  ON calls (provider);
CREATE INDEX IF NOT EXISTS idx_calls_anomaly   ON calls (cache_anomaly) WHERE cache_anomaly IS NOT NULL;
"#;

// ── Data types ────────────────────────────────────────────────────────────────

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
    pub cache_ttl: Option<String>,
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

// ── Ledger ────────────────────────────────────────────────────────────────────

pub struct Ledger {
    conn: Mutex<Connection>,
}

impl Ledger {
    /// Open (or create) a file-backed database and apply the schema.
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    /// Open an in-memory database — intended for tests only.
    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch(SCHEMA)?;
        Self::migrate(&conn)?;
        info!("Ledger schema initialized");
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Best-effort migrations for columns added after the initial schema.
    /// SQLite's ADD COLUMN is a no-op if the column already exists (we catch
    /// the "duplicate column" error and ignore it).
    fn migrate(conn: &Connection) -> Result<(), rusqlite::Error> {
        let add_if_missing = |sql: &str| -> Result<(), rusqlite::Error> {
            match conn.execute_batch(sql) {
                Ok(()) => Ok(()),
                Err(e) if e.to_string().contains("duplicate column") => Ok(()),
                Err(e) => Err(e),
            }
        };

        // v2: cache_ttl on calls
        add_if_missing(
            "ALTER TABLE calls ADD COLUMN cache_ttl TEXT DEFAULT '1h'",
        )?;

        // v3: cache_write_1h_per_token on pricing
        add_if_missing(
            "ALTER TABLE pricing ADD COLUMN cache_write_1h_per_token REAL NOT NULL DEFAULT 0.0",
        )?;

        Ok(())
    }

    /// Insert a call row, returning its autoincrement ID.
    pub fn insert(&self, row: &CallRow) -> Result<i64, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"INSERT INTO calls (
                ts, character, provider, model, call_type,
                input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                cache_ttl,
                total_ms, ttft_ms, finish_reason, thinking_enabled,
                cache_state, cache_anomaly,
                input_cost, output_cost, cache_read_cost, cache_write_cost, total_cost
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8, ?9,
                ?10,
                ?11, ?12, ?13, ?14,
                ?15, ?16,
                ?17, ?18, ?19, ?20, ?21
            )"#,
            params![
                row.ts,
                row.character,
                row.provider,
                row.model,
                row.call_type,
                row.input_tokens,
                row.output_tokens,
                row.cache_read_tokens,
                row.cache_write_tokens,
                row.cache_ttl,
                row.total_ms,
                row.ttft_ms,
                row.finish_reason,
                row.thinking_enabled as i64,
                row.cache_state,
                row.cache_anomaly,
                row.input_cost,
                row.output_cost,
                row.cache_read_cost,
                row.cache_write_cost,
                row.total_cost,
            ],
        )?;
        debug!(
            character = row.character,
            call_type = row.call_type,
            input_tokens = row.input_tokens,
            output_tokens = row.output_tokens,
            "Ledger row inserted"
        );
        Ok(conn.last_insert_rowid())
    }

    /// Return the `limit` most recent rows, newest first.
    pub fn recent(&self, limit: u32) -> Result<Vec<CallRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT * FROM calls ORDER BY id DESC LIMIT ?1")?;
        let rows = stmt
            .query_map(params![limit], row_from_sqlite)?
            .collect::<SqlResult<Vec<_>>>()?;
        debug!(count = rows.len(), limit, "Ledger recent query");
        Ok(rows)
    }

    /// Return the most recent non-compaction Anthropic call for `character`.
    pub fn last_anthropic_call(&self, character: &str) -> Result<Option<CallRow>, rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r#"SELECT * FROM calls
               WHERE character = ?1
                 AND provider  = 'anthropic'
                 AND call_type != 'compaction'
               ORDER BY id DESC
               LIMIT 1"#,
        )?;
        let mut rows = stmt.query_map(params![character], row_from_sqlite)?;
        rows.next().transpose()
    }

    /// Expose the underlying connection for sibling modules (e.g. PricingEngine).
    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }
}

// ── Row deserializer ──────────────────────────────────────────────────────────

pub(crate) fn row_from_sqlite(row: &rusqlite::Row) -> SqlResult<CallRow> {
    Ok(CallRow {
        // column 0 is the autoincrement id — skip it
        ts: row.get(1)?,
        character: row.get(2)?,
        provider: row.get(3)?,
        model: row.get(4)?,
        call_type: row.get(5)?,
        input_tokens: row.get::<_, i64>(6)? as u32,
        output_tokens: row.get::<_, i64>(7)? as u32,
        cache_read_tokens: row.get::<_, i64>(8)? as u32,
        cache_write_tokens: row.get::<_, i64>(9)? as u32,
        cache_ttl: row.get(10)?,
        total_ms: row.get::<_, i64>(11)? as u32,
        ttft_ms: row.get::<_, i64>(12)? as u32,
        finish_reason: row.get(13)?,
        thinking_enabled: row.get::<_, i64>(14)? != 0,
        cache_state: row.get(15)?,
        cache_anomaly: row.get(16)?,
        input_cost: row.get(17)?,
        output_cost: row.get(18)?,
        cache_read_cost: row.get(19)?,
        cache_write_cost: row.get(20)?,
        total_cost: row.get(21)?,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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
            cache_ttl: None,
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
        let id = ledger.insert(&sample_row()).unwrap();
        assert_eq!(id, 1);
    }

    #[test]
    fn insert_and_read_back() {
        let ledger = test_ledger();
        ledger.insert(&sample_row()).unwrap();
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
        ledger.insert(&row).unwrap();
        row.ts = "2026-04-05T12:01:00Z".into();
        row.cache_read_tokens = 120;
        ledger.insert(&row).unwrap();
        // Compaction should be excluded
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
        ledger.insert(&row).unwrap();
        let rows = ledger.recent(1).unwrap();
        assert!(rows[0].total_cost.is_none());
    }
}
