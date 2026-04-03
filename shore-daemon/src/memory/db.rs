use chrono::Utc;
use rusqlite::{params, Connection, Result as SqlResult};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS entries (
    id              TEXT PRIMARY KEY,
    memory_type     TEXT NOT NULL,
    source          TEXT NOT NULL DEFAULT '',
    reason          TEXT NOT NULL DEFAULT '',
    status          TEXT NOT NULL DEFAULT 'active',
    confidence      REAL NOT NULL DEFAULT 1.0,
    summary_text    TEXT NOT NULL DEFAULT '',
    topic_tags      TEXT NOT NULL DEFAULT '',
    topic_key       TEXT NOT NULL DEFAULT '',
    start_timestamp TEXT NOT NULL DEFAULT '',
    end_timestamp   TEXT NOT NULL DEFAULT '',
    message_count   INTEGER NOT NULL DEFAULT 0,
    source_entry_ids TEXT NOT NULL DEFAULT '',
    related_entry_ids TEXT NOT NULL DEFAULT '',
    superseded_by   TEXT NOT NULL DEFAULT '',
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    entry_type      TEXT NOT NULL DEFAULT '',
    image_path      TEXT NOT NULL DEFAULT '',
    collated_at     TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS entities (
    entity_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    type        TEXT NOT NULL DEFAULT '',
    description TEXT NOT NULL DEFAULT '',
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS entry_entities (
    entry_id    TEXT NOT NULL,
    entity_id   INTEGER NOT NULL,
    PRIMARY KEY (entry_id, entity_id),
    FOREIGN KEY (entry_id) REFERENCES entries(id),
    FOREIGN KEY (entity_id) REFERENCES entities(entity_id)
);

CREATE TABLE IF NOT EXISTS changelog (
    changelog_id INTEGER PRIMARY KEY AUTOINCREMENT,
    operation    TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    timestamp    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS changelog_entries (
    changelog_id INTEGER NOT NULL,
    entry_id     TEXT NOT NULL,
    PRIMARY KEY (changelog_id, entry_id),
    FOREIGN KEY (changelog_id) REFERENCES changelog(changelog_id),
    FOREIGN KEY (entry_id) REFERENCES entries(id)
);

CREATE TABLE IF NOT EXISTS changelog_entities (
    changelog_id INTEGER NOT NULL,
    entity_id    INTEGER NOT NULL,
    PRIMARY KEY (changelog_id, entity_id),
    FOREIGN KEY (changelog_id) REFERENCES changelog(changelog_id),
    FOREIGN KEY (entity_id) REFERENCES entities(entity_id)
);

CREATE TABLE IF NOT EXISTS flags (
    flag_id     INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id    TEXT NOT NULL,
    flag_type   TEXT NOT NULL,
    reason      TEXT NOT NULL DEFAULT '',
    resolved_at TEXT,
    resolution  TEXT,
    created_at  TEXT NOT NULL,
    FOREIGN KEY (entry_id) REFERENCES entries(id)
);

CREATE TABLE IF NOT EXISTS collation_skip (
    entry_id   TEXT NOT NULL,
    phase      TEXT NOT NULL,
    skipped_at TEXT NOT NULL,
    PRIMARY KEY (entry_id, phase)
);
";

/// Migrations for existing databases. Safe to run repeatedly (uses IF NOT EXISTS / try-ignore).
const MIGRATIONS_SQL: &str = "
-- v2.1: Add collated_at column to entries (tracks when an entry was last processed by collation).
ALTER TABLE entries ADD COLUMN collated_at TEXT NOT NULL DEFAULT '';
-- v2.3: Drop canonical column (never used by users, blocked collation on 95% of entries).
ALTER TABLE entries DROP COLUMN canonical;
";

/// FTS5 virtual table for full-text search over entries.
/// Created separately because FTS5 may not be available in all builds.
const FTS_SCHEMA_SQL: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5(
    summary_text, topic_tags, topic_key,
    content=entries, content_rowid=rowid,
    tokenize='porter unicode61'
);

-- Triggers to keep FTS index in sync with entries table.
CREATE TRIGGER IF NOT EXISTS entries_fts_insert AFTER INSERT ON entries BEGIN
    INSERT INTO entries_fts(rowid, summary_text, topic_tags, topic_key)
    VALUES (new.rowid, new.summary_text, new.topic_tags, new.topic_key);
END;

CREATE TRIGGER IF NOT EXISTS entries_fts_update AFTER UPDATE ON entries BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, summary_text, topic_tags, topic_key)
    VALUES ('delete', old.rowid, old.summary_text, old.topic_tags, old.topic_key);
    INSERT INTO entries_fts(rowid, summary_text, topic_tags, topic_key)
    VALUES (new.rowid, new.summary_text, new.topic_tags, new.topic_key);
END;

CREATE TRIGGER IF NOT EXISTS entries_fts_delete AFTER DELETE ON entries BEGIN
    INSERT INTO entries_fts(entries_fts, rowid, summary_text, topic_tags, topic_key)
    VALUES ('delete', old.rowid, old.summary_text, old.topic_tags, old.topic_key);
END;
";

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Entry {
    pub id: String,
    pub memory_type: String,
    pub source: String,
    pub reason: String,
    pub status: String,
    pub confidence: f64,
    pub summary_text: String,
    pub topic_tags: String,
    pub topic_key: String,
    pub start_timestamp: String,
    pub end_timestamp: String,
    pub message_count: i64,
    pub source_entry_ids: String,
    pub related_entry_ids: String,
    pub superseded_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub entry_type: String,
    pub image_path: String,
    /// When this entry was last processed by collation. Empty = never collated.
    pub collated_at: String,
}

#[derive(Debug, Clone)]
pub struct Entity {
    pub entity_id: i64,
    pub name: String,
    pub entity_type: String,
    pub description: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone)]
pub struct ChangelogRecord {
    pub changelog_id: i64,
    pub operation: String,
    pub description: String,
    pub timestamp: String,
}

#[derive(Debug, Clone)]
pub struct Flag {
    pub flag_id: i64,
    pub entry_id: String,
    pub flag_type: String,
    pub reason: String,
    pub resolved_at: Option<String>,
    pub resolution: Option<String>,
    pub created_at: String,
}

/// A single hit from the FTS5 full-text search.
#[derive(Debug, Clone)]
pub struct FtsHit {
    pub entry_id: String,
    pub rank: f64,
    pub summary_text: String,
    pub topic_tags: String,
    pub topic_key: String,
    pub status: String,
    pub confidence: f64,
    pub memory_type: String,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct CollationSkip {
    pub entry_id: String,
    pub phase: String,
    pub skipped_at: String,
}

// ---------------------------------------------------------------------------
// MemoryDB
// ---------------------------------------------------------------------------

pub struct MemoryDB {
    conn: Connection,
}

// SAFETY: MemoryDB (rusqlite::Connection) is Send but not Sync because SQLite
// connections do not support *concurrent* access from multiple threads. In the
// shore-daemon, MemoryDB is only ever used from a single tokio task at a time
// (the message handler processes requests sequentially), so concurrent access
// never occurs. This impl is needed because &MemoryDB must be Send to cross
// .await points inside Send futures spawned by tokio::spawn.
unsafe impl Sync for MemoryDB {}

impl MemoryDB {
    /// Open (or create) the database at the given path and run auto-migration.
    pub fn open(path: &Path) -> SqlResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CANTOPEN),
                    Some(format!("cannot create directory: {e}")),
                )
            })?;
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA_SQL)?;
        Self::run_migrations(&conn);
        // FTS5 is best-effort — don't fail if the extension isn't available.
        if conn.execute_batch(FTS_SCHEMA_SQL).is_ok() {
            Self::populate_fts_if_empty(&conn);
        }
        Ok(Self { conn })
    }

    /// Open an in-memory database (useful for tests).
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA_SQL)?;
        Self::run_migrations(&conn);
        let _ = conn.execute_batch(FTS_SCHEMA_SQL);
        Ok(Self { conn })
    }

    /// Open an existing V1 database without running migration (schema-compatible).
    pub fn open_v1(path: &Path) -> SqlResult<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        // V1 schema is identical — no migration needed.
        // We still run CREATE IF NOT EXISTS so missing tables are harmless.
        conn.execute_batch(SCHEMA_SQL)?;
        Self::run_migrations(&conn);
        if conn.execute_batch(FTS_SCHEMA_SQL).is_ok() {
            Self::populate_fts_if_empty(&conn);
        }
        Ok(Self { conn })
    }

    /// Run idempotent migrations. Each statement is tried individually;
    /// failures (e.g. "duplicate column") are silently ignored.
    fn run_migrations(conn: &Connection) {
        for stmt in MIGRATIONS_SQL.lines() {
            let trimmed = stmt.trim();
            if trimmed.is_empty() || trimmed.starts_with("--") {
                continue;
            }
            let _ = conn.execute_batch(trimmed);
        }

        // v2.2: Fix FTS schema mismatch. Old databases may have a 5-column FTS
        // table (entry_id, summary_text, topic_tags, topic_key, entities_text)
        // from V1 import. The current schema uses 3 columns. Detect by checking
        // if the creation SQL contains "entry_id" and drop/recreate if so.
        let needs_fts_rebuild: bool = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'entries_fts'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|sql| sql.contains("entry_id"))
            .unwrap_or(false);

        if needs_fts_rebuild {
            let _ = conn.execute_batch(
                "DROP TRIGGER IF EXISTS entries_fts_insert;
                 DROP TRIGGER IF EXISTS entries_fts_update;
                 DROP TRIGGER IF EXISTS entries_fts_delete;
                 DROP TABLE IF EXISTS entries_fts;",
            );
        }
    }

    /// Populate FTS index from entries if the index is empty but entries exist.
    /// This handles the case where the FTS table was just recreated by migration.
    fn populate_fts_if_empty(conn: &Connection) {
        let fts_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries_fts", [], |r| r.get(0))
            .unwrap_or(0);
        let entry_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .unwrap_or(0);
        if fts_count == 0 && entry_count > 0 {
            let _ = conn.execute_batch(
                "INSERT INTO entries_fts(rowid, summary_text, topic_tags, topic_key)
                   SELECT rowid, summary_text, topic_tags, topic_key FROM entries;",
            );
        }
    }

    /// Resolve the default database path for a character.
    pub fn default_path(character: &str) -> PathBuf {
        shore_config::data_dir()
            .join(character)
            .join("memory")
            .join("memory.db")
    }

    // ------------------------------------------------------------------
    // Entries
    // ------------------------------------------------------------------

    pub fn create_entry(&self, entry: &Entry) -> SqlResult<()> {
        self.conn.execute(
            "INSERT INTO entries (
                id, memory_type, source, reason, status, confidence,
                summary_text, topic_tags, topic_key, start_timestamp, end_timestamp,
                message_count, source_entry_ids, related_entry_ids, superseded_by,
                created_at, updated_at, entry_type, image_path, collated_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20
            )",
            params![
                entry.id,
                entry.memory_type,
                entry.source,
                entry.reason,
                entry.status,
                entry.confidence,
                entry.summary_text,
                entry.topic_tags,
                entry.topic_key,
                entry.start_timestamp,
                entry.end_timestamp,
                entry.message_count,
                entry.source_entry_ids,
                entry.related_entry_ids,
                entry.superseded_by,
                entry.created_at,
                entry.updated_at,
                entry.entry_type,
                entry.image_path,
                entry.collated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_entry(&self, id: &str) -> SqlResult<Option<Entry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, memory_type, source, reason, status, confidence,
                    summary_text, topic_tags, topic_key, start_timestamp, end_timestamp,
                    message_count, source_entry_ids, related_entry_ids, superseded_by,
                    created_at, updated_at, entry_type, image_path, collated_at
             FROM entries WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], row_to_entry)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_entries_by_status(&self, status: &str) -> SqlResult<Vec<Entry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, memory_type, source, reason, status, confidence,
                    summary_text, topic_tags, topic_key, start_timestamp, end_timestamp,
                    message_count, source_entry_ids, related_entry_ids, superseded_by,
                    created_at, updated_at, entry_type, image_path, collated_at
             FROM entries WHERE status = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![status], row_to_entry)?;
        rows.collect()
    }

    /// Count entries by status (more efficient than fetching all rows).
    pub fn count_entries_by_status(&self, status: &str) -> SqlResult<i64> {
        self.conn.query_row(
            "SELECT COUNT(*) FROM entries WHERE status = ?1",
            params![status],
            |row| row.get(0),
        )
    }

    /// Count all entries regardless of status.
    pub fn count_entries(&self) -> SqlResult<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM entries", [], |row| row.get(0))
    }

    /// Count all entities.
    pub fn count_entities(&self) -> SqlResult<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
    }

    pub fn get_entries_by_type(&self, memory_type: &str) -> SqlResult<Vec<Entry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, memory_type, source, reason, status, confidence,
                    summary_text, topic_tags, topic_key, start_timestamp, end_timestamp,
                    message_count, source_entry_ids, related_entry_ids, superseded_by,
                    created_at, updated_at, entry_type, image_path, collated_at
             FROM entries WHERE memory_type = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![memory_type], row_to_entry)?;
        rows.collect()
    }

    pub fn update_entry(&self, entry: &Entry) -> SqlResult<usize> {
        self.conn.execute(
            "UPDATE entries SET
                memory_type = ?2, source = ?3, reason = ?4, status = ?5,
                confidence = ?6, summary_text = ?7,
                topic_tags = ?8, topic_key = ?9, start_timestamp = ?10,
                end_timestamp = ?11, message_count = ?12, source_entry_ids = ?13,
                related_entry_ids = ?14, superseded_by = ?15,
                updated_at = ?16, entry_type = ?17, image_path = ?18,
                collated_at = ?19
             WHERE id = ?1",
            params![
                entry.id,
                entry.memory_type,
                entry.source,
                entry.reason,
                entry.status,
                entry.confidence,
                entry.summary_text,
                entry.topic_tags,
                entry.topic_key,
                entry.start_timestamp,
                entry.end_timestamp,
                entry.message_count,
                entry.source_entry_ids,
                entry.related_entry_ids,
                entry.superseded_by,
                entry.updated_at,
                entry.entry_type,
                entry.image_path,
                entry.collated_at,
            ],
        )
    }

    /// Mark an entry as superseded and point it at the replacement entry.
    pub fn supersede_entry(&self, old_id: &str, new_id: &str) -> SqlResult<usize> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE entries SET status = 'superseded', superseded_by = ?2, updated_at = ?3
             WHERE id = ?1",
            params![old_id, new_id, now],
        )
    }

    /// Permanently delete an entry by ID.
    pub fn delete_entry(&self, id: &str) -> SqlResult<usize> {
        // Clean up FK references before deleting.
        self.conn.execute("DELETE FROM entry_entities WHERE entry_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM changelog_entries WHERE entry_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM flags WHERE entry_id = ?1", params![id])?;
        self.conn
            .execute("DELETE FROM entries WHERE id = ?1", params![id])
    }

    /// Run VACUUM to reclaim disk space after bulk deletes.
    pub fn vacuum(&self) -> SqlResult<()> {
        self.conn.execute_batch("VACUUM")?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Entities
    // ------------------------------------------------------------------

    /// Upsert an entity. If the description changes, a changelog record is
    /// appended automatically.
    pub fn upsert_entity(
        &self,
        name: &str,
        entity_type: &str,
        description: &str,
    ) -> SqlResult<i64> {
        let now = Utc::now().to_rfc3339();

        // Check for existing entity.
        let existing: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT entity_id, description FROM entities WHERE name = ?1 COLLATE NOCASE",
                params![name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .ok();

        match existing {
            Some((entity_id, old_desc)) => {
                self.conn.execute(
                    "UPDATE entities SET type = ?2, description = ?3, updated_at = ?4
                     WHERE entity_id = ?1",
                    params![entity_id, entity_type, description, now],
                )?;

                // Log description change.
                if old_desc != description {
                    let cl_id = self.append_changelog(
                        "entity_description_change",
                        &format!(
                            "Entity '{}' description changed from '{}' to '{}'",
                            name, old_desc, description
                        ),
                    )?;
                    self.conn.execute(
                        "INSERT INTO changelog_entities (changelog_id, entity_id) VALUES (?1, ?2)",
                        params![cl_id, entity_id],
                    )?;
                }

                Ok(entity_id)
            }
            None => {
                self.conn.execute(
                    "INSERT INTO entities (name, type, description, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![name, entity_type, description, now, now],
                )?;
                Ok(self.conn.last_insert_rowid())
            }
        }
    }

    pub fn get_entity(&self, entity_id: i64) -> SqlResult<Option<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT entity_id, name, type, description, created_at, updated_at
             FROM entities WHERE entity_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![entity_id], row_to_entity)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_entity_by_name(&self, name: &str) -> SqlResult<Option<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT entity_id, name, type, description, created_at, updated_at
             FROM entities WHERE name = ?1 COLLATE NOCASE",
        )?;
        let mut rows = stmt.query_map(params![name], row_to_entity)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Link an entity to an entry (entry_entities junction).
    pub fn link_entity_to_entry(&self, entry_id: &str, entity_id: i64) -> SqlResult<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO entry_entities (entry_id, entity_id) VALUES (?1, ?2)",
            params![entry_id, entity_id],
        )?;
        Ok(())
    }

    /// Get all entities.
    pub fn get_all_entities(&self) -> SqlResult<Vec<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT entity_id, name, type, description, created_at, updated_at
             FROM entities ORDER BY name",
        )?;
        let rows = stmt.query_map([], row_to_entity)?;
        rows.collect()
    }

    /// Reassign all entry_entities links from one entity to another.
    pub fn reassign_entity_links(&self, from_id: i64, to_id: i64) -> SqlResult<()> {
        // First, delete links that would cause duplicates (entry already linked to target).
        self.conn.execute(
            "DELETE FROM entry_entities
             WHERE entity_id = ?1
               AND entry_id IN (SELECT entry_id FROM entry_entities WHERE entity_id = ?2)",
            params![from_id, to_id],
        )?;
        // Then reassign remaining links.
        self.conn.execute(
            "UPDATE entry_entities SET entity_id = ?2 WHERE entity_id = ?1",
            params![from_id, to_id],
        )?;
        Ok(())
    }

    /// Delete an entity by ID.
    pub fn delete_entity(&self, entity_id: i64) -> SqlResult<()> {
        self.conn.execute(
            "DELETE FROM entry_entities WHERE entity_id = ?1",
            params![entity_id],
        )?;
        self.conn.execute(
            "DELETE FROM changelog_entities WHERE entity_id = ?1",
            params![entity_id],
        )?;
        self.conn.execute(
            "DELETE FROM entities WHERE entity_id = ?1",
            params![entity_id],
        )?;
        Ok(())
    }

    /// Get all entities linked to an entry.
    pub fn get_entities_for_entry(&self, entry_id: &str) -> SqlResult<Vec<Entity>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.entity_id, e.name, e.type, e.description, e.created_at, e.updated_at
             FROM entities e
             JOIN entry_entities ee ON e.entity_id = ee.entity_id
             WHERE ee.entry_id = ?1",
        )?;
        let rows = stmt.query_map(params![entry_id], row_to_entity)?;
        rows.collect()
    }

    // ------------------------------------------------------------------
    // Changelog
    // ------------------------------------------------------------------

    /// Append a changelog record. Returns the new changelog_id.
    pub fn append_changelog(&self, operation: &str, description: &str) -> SqlResult<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO changelog (operation, description, timestamp) VALUES (?1, ?2, ?3)",
            params![operation, description, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Link a changelog record to an entry.
    pub fn link_changelog_entry(&self, changelog_id: i64, entry_id: &str) -> SqlResult<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO changelog_entries (changelog_id, entry_id) VALUES (?1, ?2)",
            params![changelog_id, entry_id],
        )?;
        Ok(())
    }

    /// Link a changelog record to an entity.
    pub fn link_changelog_entity(&self, changelog_id: i64, entity_id: i64) -> SqlResult<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO changelog_entities (changelog_id, entity_id) VALUES (?1, ?2)",
            params![changelog_id, entity_id],
        )?;
        Ok(())
    }

    /// Query recent changelog records (most recent first).
    pub fn get_recent_changelog(&self, limit: i64) -> SqlResult<Vec<ChangelogRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT changelog_id, operation, description, timestamp
             FROM changelog ORDER BY changelog_id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(ChangelogRecord {
                changelog_id: row.get(0)?,
                operation: row.get(1)?,
                description: row.get(2)?,
                timestamp: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    // ------------------------------------------------------------------
    // Flags
    // ------------------------------------------------------------------

    pub fn create_flag(
        &self,
        entry_id: &str,
        flag_type: &str,
        reason: &str,
    ) -> SqlResult<i64> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO flags (entry_id, flag_type, reason, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![entry_id, flag_type, reason, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn resolve_flag(&self, flag_id: i64, resolution: &str) -> SqlResult<usize> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE flags SET resolved_at = ?2, resolution = ?3 WHERE flag_id = ?1",
            params![flag_id, now, resolution],
        )
    }

    pub fn get_unresolved_flags_by_type(&self, flag_type: &str) -> SqlResult<Vec<Flag>> {
        let mut stmt = self.conn.prepare(
            "SELECT flag_id, entry_id, flag_type, reason, resolved_at, resolution, created_at
             FROM flags WHERE flag_type = ?1 AND resolved_at IS NULL
             ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![flag_type], row_to_flag)?;
        rows.collect()
    }

    pub fn get_flags_for_entry(&self, entry_id: &str) -> SqlResult<Vec<Flag>> {
        let mut stmt = self.conn.prepare(
            "SELECT flag_id, entry_id, flag_type, reason, resolved_at, resolution, created_at
             FROM flags WHERE entry_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![entry_id], row_to_flag)?;
        rows.collect()
    }

    // ------------------------------------------------------------------
    // Collation skip
    // ------------------------------------------------------------------

    pub fn add_collation_skip(&self, entry_id: &str, phase: &str) -> SqlResult<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT OR IGNORE INTO collation_skip (entry_id, phase, skipped_at)
             VALUES (?1, ?2, ?3)",
            params![entry_id, phase, now],
        )?;
        Ok(())
    }

    pub fn is_collation_skipped(&self, entry_id: &str, phase: &str) -> SqlResult<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM collation_skip WHERE entry_id = ?1 AND phase = ?2",
            params![entry_id, phase],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    pub fn get_collation_skips(&self, phase: &str) -> SqlResult<Vec<CollationSkip>> {
        let mut stmt = self.conn.prepare(
            "SELECT entry_id, phase, skipped_at FROM collation_skip WHERE phase = ?1",
        )?;
        let rows = stmt.query_map(params![phase], |row| {
            Ok(CollationSkip {
                entry_id: row.get(0)?,
                phase: row.get(1)?,
                skipped_at: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    // ------------------------------------------------------------------
    // Full-text search (FTS5)
    // ------------------------------------------------------------------

    /// Full-text search over memory entries using FTS5.
    ///
    /// Uses stemming and relevance ranking — better than SQL LIKE for finding
    /// entries by content, topic, or person name. Returns up to `limit` results.
    pub fn search_entries_fts(
        &self,
        query: &str,
        status: Option<&str>,
        limit: usize,
    ) -> SqlResult<Vec<FtsHit>> {
        let sql = match status {
            Some("all") | None => {
                "SELECT e.id, rank, e.summary_text, e.topic_tags, e.topic_key,
                        e.status, e.confidence, e.memory_type, e.created_at
                 FROM entries_fts
                 JOIN entries e ON e.rowid = entries_fts.rowid
                 WHERE entries_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2"
            }
            Some(_) => {
                "SELECT e.id, rank, e.summary_text, e.topic_tags, e.topic_key,
                        e.status, e.confidence, e.memory_type, e.created_at
                 FROM entries_fts
                 JOIN entries e ON e.rowid = entries_fts.rowid
                 WHERE entries_fts MATCH ?1 AND e.status = ?3
                 ORDER BY rank
                 LIMIT ?2"
            }
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = if let Some(s) = status.filter(|s| *s != "all") {
            stmt.query_map(params![query, limit as i64, s], row_to_fts_hit)?
        } else {
            stmt.query_map(params![query, limit as i64], row_to_fts_hit)?
        };
        rows.collect()
    }

    // ------------------------------------------------------------------
    // FTS maintenance
    // ------------------------------------------------------------------

    /// Rebuild the FTS index from scratch (drop + recreate + repopulate).
    /// This is more robust than DELETE + INSERT when the existing FTS data
    /// is corrupted or was built by an incompatible SQLite version.
    pub fn rebuild_fts(&self) -> SqlResult<()> {
        self.conn.execute_batch(
            "DROP TRIGGER IF EXISTS entries_fts_insert;
             DROP TRIGGER IF EXISTS entries_fts_update;
             DROP TRIGGER IF EXISTS entries_fts_delete;
             DROP TABLE IF EXISTS entries_fts;",
        )?;
        self.conn.execute_batch(FTS_SCHEMA_SQL)?;
        self.conn.execute_batch(
            "INSERT INTO entries_fts(rowid, summary_text, topic_tags, topic_key)
               SELECT rowid, summary_text, topic_tags, topic_key FROM entries;",
        )
    }

    // ------------------------------------------------------------------
    // Read-only SQL query
    // ------------------------------------------------------------------

    /// Execute an arbitrary read-only SQL query. Only SELECT statements are
    /// allowed. Returns up to `max_rows` rows as JSON-friendly maps.
    pub fn query_db_readonly(
        &self,
        sql: &str,
        max_rows: usize,
    ) -> SqlResult<Vec<HashMap<String, serde_json::Value>>> {
        let trimmed = sql.trim();
        if !trimmed
            .split_whitespace()
            .next()
            .map_or(false, |w| w.eq_ignore_ascii_case("SELECT"))
        {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
                Some("Only SELECT queries are allowed".to_string()),
            ));
        }

        let mut stmt = self.conn.prepare(trimmed)?;
        let column_names: Vec<String> = stmt
            .column_names()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let mut results = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if results.len() >= max_rows {
                break;
            }
            let mut map = HashMap::new();
            for (i, col_name) in column_names.iter().enumerate() {
                let val = row_value_to_json(row, i);
                map.insert(col_name.clone(), val);
            }
            results.push(map);
        }
        Ok(results)
    }

    // ------------------------------------------------------------------
    // Additional convenience methods
    // ------------------------------------------------------------------

    /// Merge one entity into another: reassign all entry links from
    /// `from_id` to `to_id`, then delete the source entity.
    /// Returns the count of reassigned entry links.
    pub fn merge_entity(&self, from_id: i64, to_id: i64) -> SqlResult<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM entry_entities WHERE entity_id = ?1",
            params![from_id],
            |row| row.get(0),
        )?;
        self.reassign_entity_links(from_id, to_id)?;
        self.delete_entity(from_id)?;
        Ok(count as usize)
    }

    /// Look up a single flag by ID.
    pub fn get_flag(&self, flag_id: i64) -> SqlResult<Option<Flag>> {
        let mut stmt = self.conn.prepare(
            "SELECT flag_id, entry_id, flag_type, reason, resolved_at, resolution, created_at
             FROM flags WHERE flag_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![flag_id], row_to_flag)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Stamp an entry as collated at the given timestamp.
    /// This marks the entry as processed by collation in its current form.
    pub fn stamp_collated(&self, entry_id: &str, timestamp: &str) -> SqlResult<usize> {
        self.conn.execute(
            "UPDATE entries SET collated_at = ?2 WHERE id = ?1",
            params![entry_id, timestamp],
        )
    }

    /// Update an entry's confidence score.
    pub fn set_confidence(&self, entry_id: &str, confidence: f64) -> SqlResult<usize> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE entries SET confidence = ?2, updated_at = ?3 WHERE id = ?1",
            params![entry_id, confidence, now],
        )
    }

    /// Get all entries linked to a specific entity.
    pub fn get_entries_for_entity(&self, entity_id: i64) -> SqlResult<Vec<Entry>> {
        let mut stmt = self.conn.prepare(
            "SELECT e.id, e.memory_type, e.source, e.reason, e.status, e.confidence,
                    e.summary_text, e.topic_tags, e.topic_key, e.start_timestamp, e.end_timestamp,
                    e.message_count, e.source_entry_ids, e.related_entry_ids, e.superseded_by,
                    e.created_at, e.updated_at, e.entry_type, e.image_path, e.collated_at
             FROM entries e
             JOIN entry_entities ee ON e.id = ee.entry_id
             WHERE ee.entity_id = ?1",
        )?;
        let rows = stmt.query_map(params![entity_id], row_to_entry)?;
        rows.collect()
    }
}

// ---------------------------------------------------------------------------
// Row mappers
// ---------------------------------------------------------------------------

fn row_to_entry(row: &rusqlite::Row<'_>) -> SqlResult<Entry> {
    Ok(Entry {
        id: row.get(0)?,
        memory_type: row.get(1)?,
        source: row.get(2)?,
        reason: row.get(3)?,
        status: row.get(4)?,
        confidence: row.get(5)?,
        summary_text: row.get(6)?,
        topic_tags: row.get(7)?,
        topic_key: row.get(8)?,
        start_timestamp: row.get(9)?,
        end_timestamp: row.get(10)?,
        message_count: row.get(11)?,
        source_entry_ids: row.get(12)?,
        related_entry_ids: row.get(13)?,
        superseded_by: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        entry_type: row.get(17)?,
        image_path: row.get(18)?,
        collated_at: row.get(19)?,
    })
}

fn row_to_entity(row: &rusqlite::Row<'_>) -> SqlResult<Entity> {
    Ok(Entity {
        entity_id: row.get(0)?,
        name: row.get(1)?,
        entity_type: row.get(2)?,
        description: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn row_to_fts_hit(row: &rusqlite::Row<'_>) -> SqlResult<FtsHit> {
    Ok(FtsHit {
        entry_id: row.get(0)?,
        rank: row.get(1)?,
        summary_text: row.get(2)?,
        topic_tags: row.get(3)?,
        topic_key: row.get(4)?,
        status: row.get(5)?,
        confidence: row.get(6)?,
        memory_type: row.get(7)?,
        created_at: row.get(8)?,
    })
}

/// Convert a rusqlite row value at column `idx` to a serde_json::Value.
fn row_value_to_json(row: &rusqlite::Row<'_>, idx: usize) -> serde_json::Value {
    use rusqlite::types::ValueRef;
    match row.get_ref(idx) {
        Ok(ValueRef::Null) => serde_json::Value::Null,
        Ok(ValueRef::Integer(i)) => serde_json::json!(i),
        Ok(ValueRef::Real(f)) => serde_json::json!(f),
        Ok(ValueRef::Text(t)) => {
            serde_json::Value::String(String::from_utf8_lossy(t).into_owned())
        }
        Ok(ValueRef::Blob(b)) => serde_json::json!(format!("<blob {} bytes>", b.len())),
        Err(_) => serde_json::Value::Null,
    }
}

fn row_to_flag(row: &rusqlite::Row<'_>) -> SqlResult<Flag> {
    Ok(Flag {
        flag_id: row.get(0)?,
        entry_id: row.get(1)?,
        flag_type: row.get(2)?,
        reason: row.get(3)?,
        resolved_at: row.get(4)?,
        resolution: row.get(5)?,
        created_at: row.get(6)?,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_entry(id: &str, memory_type: &str) -> Entry {
        let now = Utc::now().to_rfc3339();
        Entry {
            id: id.to_string(),
            memory_type: memory_type.to_string(),
            source: "summary".to_string(),
            reason: "compaction".to_string(),
            status: "active".to_string(),

            confidence: 0.9,
            summary_text: "Test memory entry".to_string(),
            topic_tags: "test,memory".to_string(),
            topic_key: "testing".to_string(),
            start_timestamp: now.clone(),
            end_timestamp: now.clone(),
            message_count: 5,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: now.clone(),
            updated_at: now,
            entry_type: String::new(),
            image_path: String::new(),
            collated_at: String::new(),
        }
    }

    #[test]
    fn test_open_in_memory() {
        let db = MemoryDB::open_in_memory().unwrap();
        // Verify tables exist by querying them.
        let count: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM entries", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_open_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.db");
        let db = MemoryDB::open(&path).unwrap();
        db.create_entry(&make_entry("20250101_120000_0", "episodic"))
            .unwrap();
        drop(db);

        // Re-open and verify data persists.
        let db2 = MemoryDB::open(&path).unwrap();
        let entry = db2.get_entry("20250101_120000_0").unwrap();
        assert!(entry.is_some());
    }

    // -- Entry CRUD -------------------------------------------------------

    #[test]
    fn test_entry_crud_cycle() {
        let db = MemoryDB::open_in_memory().unwrap();

        // Create
        let e1 = make_entry("20250101_120000_0", "episodic");
        db.create_entry(&e1).unwrap();

        // Read by ID
        let fetched = db.get_entry("20250101_120000_0").unwrap().unwrap();
        assert_eq!(fetched.memory_type, "episodic");
        assert_eq!(fetched.summary_text, "Test memory entry");

        // Read by status
        let active = db.get_entries_by_status("active").unwrap();
        assert_eq!(active.len(), 1);

        // Read by type
        let episodic = db.get_entries_by_type("episodic").unwrap();
        assert_eq!(episodic.len(), 1);

        // Update
        let mut updated = fetched;
        updated.summary_text = "Updated text".to_string();
        updated.updated_at = Utc::now().to_rfc3339();
        db.update_entry(&updated).unwrap();
        let re_fetched = db.get_entry("20250101_120000_0").unwrap().unwrap();
        assert_eq!(re_fetched.summary_text, "Updated text");

        // Supersede
        let e2 = make_entry("20250101_130000_0", "episodic");
        db.create_entry(&e2).unwrap();
        db.supersede_entry("20250101_120000_0", "20250101_130000_0")
            .unwrap();
        let superseded = db.get_entry("20250101_120000_0").unwrap().unwrap();
        assert_eq!(superseded.status, "superseded");
        assert_eq!(superseded.superseded_by, "20250101_130000_0");

        // Verify status filter now reflects the change.
        let active = db.get_entries_by_status("active").unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "20250101_130000_0");
    }

    #[test]
    fn test_entry_not_found() {
        let db = MemoryDB::open_in_memory().unwrap();
        assert!(db.get_entry("nonexistent").unwrap().is_none());
    }

    // -- Entity CRUD ------------------------------------------------------

    #[test]
    fn test_entity_upsert_and_changelog() {
        let db = MemoryDB::open_in_memory().unwrap();

        // Create
        let id = db.upsert_entity("Alice", "person", "A friend").unwrap();
        let entity = db.get_entity(id).unwrap().unwrap();
        assert_eq!(entity.name, "Alice");
        assert_eq!(entity.description, "A friend");

        // Upsert with new description — should log changelog.
        let id2 = db
            .upsert_entity("Alice", "person", "A close friend")
            .unwrap();
        assert_eq!(id, id2);

        let entity = db.get_entity(id).unwrap().unwrap();
        assert_eq!(entity.description, "A close friend");

        // Verify changelog was created for the description change.
        let logs = db.get_recent_changelog(10).unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].operation, "entity_description_change");
        assert!(logs[0].description.contains("Alice"));

        // Case-insensitive lookup.
        let by_name = db.get_entity_by_name("alice").unwrap().unwrap();
        assert_eq!(by_name.entity_id, id);
    }

    #[test]
    fn test_entity_link_to_entry() {
        let db = MemoryDB::open_in_memory().unwrap();

        let entry = make_entry("20250101_120000_0", "episodic");
        db.create_entry(&entry).unwrap();

        let eid = db.upsert_entity("Bob", "person", "A colleague").unwrap();
        db.link_entity_to_entry("20250101_120000_0", eid).unwrap();

        let entities = db.get_entities_for_entry("20250101_120000_0").unwrap();
        assert_eq!(entities.len(), 1);
        assert_eq!(entities[0].name, "Bob");

        // Duplicate link should be ignored.
        db.link_entity_to_entry("20250101_120000_0", eid).unwrap();
        let entities = db.get_entities_for_entry("20250101_120000_0").unwrap();
        assert_eq!(entities.len(), 1);
    }

    // -- Changelog --------------------------------------------------------

    #[test]
    fn test_changelog_crud() {
        let db = MemoryDB::open_in_memory().unwrap();

        let entry = make_entry("20250101_120000_0", "episodic");
        db.create_entry(&entry).unwrap();

        let cl_id = db.append_changelog("create_entry", "Created entry").unwrap();
        db.link_changelog_entry(cl_id, "20250101_120000_0").unwrap();

        let eid = db.upsert_entity("Charlie", "person", "Test").unwrap();
        let cl_id2 = db
            .append_changelog("create_entity", "Created entity Charlie")
            .unwrap();
        db.link_changelog_entity(cl_id2, eid).unwrap();

        let logs = db.get_recent_changelog(10).unwrap();
        assert_eq!(logs.len(), 2);
        // Most recent first.
        assert_eq!(logs[0].operation, "create_entity");
        assert_eq!(logs[1].operation, "create_entry");
    }

    // -- Flags ------------------------------------------------------------

    #[test]
    fn test_flags_crud() {
        let db = MemoryDB::open_in_memory().unwrap();

        let entry = make_entry("20250101_120000_0", "episodic");
        db.create_entry(&entry).unwrap();

        // Create flags
        let f1 = db
            .create_flag("20250101_120000_0", "low_confidence", "Confidence below threshold")
            .unwrap();
        let f2 = db
            .create_flag("20250101_120000_0", "duplicate", "Possible duplicate")
            .unwrap();

        // Query unresolved by type.
        let unresolved = db.get_unresolved_flags_by_type("low_confidence").unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].flag_id, f1);

        // Get flags for entry.
        let entry_flags = db.get_flags_for_entry("20250101_120000_0").unwrap();
        assert_eq!(entry_flags.len(), 2);

        // Resolve a flag.
        db.resolve_flag(f1, "Confidence recalculated").unwrap();
        let unresolved = db.get_unresolved_flags_by_type("low_confidence").unwrap();
        assert_eq!(unresolved.len(), 0);

        // f2 should still be unresolved.
        let unresolved = db.get_unresolved_flags_by_type("duplicate").unwrap();
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].flag_id, f2);
    }

    // -- Collation skip ---------------------------------------------------

    #[test]
    fn test_collation_skip() {
        let db = MemoryDB::open_in_memory().unwrap();

        let entry = make_entry("20250101_120000_0", "episodic");
        db.create_entry(&entry).unwrap();

        assert!(!db
            .is_collation_skipped("20250101_120000_0", "phase1")
            .unwrap());

        db.add_collation_skip("20250101_120000_0", "phase1")
            .unwrap();
        assert!(db
            .is_collation_skipped("20250101_120000_0", "phase1")
            .unwrap());

        // Duplicate insert should be ignored.
        db.add_collation_skip("20250101_120000_0", "phase1")
            .unwrap();

        let skips = db.get_collation_skips("phase1").unwrap();
        assert_eq!(skips.len(), 1);
    }

    // -- V1 compatibility -------------------------------------------------

    #[test]
    fn test_v1_database_compatibility() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("v1.db");

        // Simulate a V1 database by creating the schema directly.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA_SQL).unwrap();

            // Insert a V1-style entry.
            conn.execute(
                "INSERT INTO entries (
                    id, memory_type, source, reason, status, confidence,
                    summary_text, topic_tags, topic_key, start_timestamp, end_timestamp,
                    message_count, source_entry_ids, related_entry_ids, superseded_by,
                    created_at, updated_at, entry_type, image_path, collated_at
                ) VALUES (
                    '20240601_100000_0', 'episodic', 'summary', 'compaction', 'active', 0.85,
                    'V1 memory content', 'v1,test', 'legacy', '2024-06-01T10:00:00Z', '2024-06-01T10:30:00Z',
                    10, '', '', '',
                    '2024-06-01T10:00:00Z', '2024-06-01T10:00:00Z', '', '', ''
                )",
                [],
            )
            .unwrap();

            conn.execute(
                "INSERT INTO entities (name, type, description, created_at, updated_at)
                 VALUES ('V1Entity', 'concept', 'From V1', '2024-06-01T10:00:00Z', '2024-06-01T10:00:00Z')",
                [],
            )
            .unwrap();
        }

        // Open with V2 code — should work without migration.
        let db = MemoryDB::open_v1(&path).unwrap();

        let entry = db.get_entry("20240601_100000_0").unwrap().unwrap();
        assert_eq!(entry.summary_text, "V1 memory content");
        assert_eq!(entry.memory_type, "episodic");
        assert_eq!(entry.confidence, 0.85);

        let entity = db.get_entity_by_name("V1Entity").unwrap().unwrap();
        assert_eq!(entity.description, "From V1");

        // V2 operations should work on the V1 database.
        let new_entry = make_entry("20250325_120000_0", "semantic");
        db.create_entry(&new_entry).unwrap();
        assert!(db.get_entry("20250325_120000_0").unwrap().is_some());
    }

    // -- Default path -----------------------------------------------------

    #[test]
    fn test_default_path() {
        let path = MemoryDB::default_path("shore");
        assert!(path.ends_with("shore/shore/memory/memory.db"));
    }

    // -- stamp_collated / get_collation_skips --------------------------------

    #[test]
    fn test_stamp_collated_direct() {
        let db = MemoryDB::open_in_memory().unwrap();
        let entry = make_entry("20250401_120000_0", "episodic");
        db.create_entry(&entry).unwrap();

        // Stamp and verify row count.
        let rows = db.stamp_collated("20250401_120000_0", "2026-04-03T12:00:00Z").unwrap();
        assert_eq!(rows, 1);

        // Verify the field was updated.
        let fetched = db.get_entry("20250401_120000_0").unwrap().unwrap();
        assert_eq!(fetched.collated_at, "2026-04-03T12:00:00Z");

        // Non-existent ID returns 0.
        let rows = db.stamp_collated("nonexistent", "2026-04-03T12:00:00Z").unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn test_get_collation_skips_filters_by_phase() {
        let db = MemoryDB::open_in_memory().unwrap();
        let entry = make_entry("20250401_120000_0", "episodic");
        db.create_entry(&entry).unwrap();

        // Add skips for two different phases.
        db.add_collation_skip("20250401_120000_0", "refine").unwrap();
        db.add_collation_skip("20250401_120000_0", "decay").unwrap();

        // Query by phase.
        let refine_skips = db.get_collation_skips("refine").unwrap();
        assert_eq!(refine_skips.len(), 1);
        assert_eq!(refine_skips[0].entry_id, "20250401_120000_0");
        assert_eq!(refine_skips[0].phase, "refine");
        assert!(!refine_skips[0].skipped_at.is_empty());

        let decay_skips = db.get_collation_skips("decay").unwrap();
        assert_eq!(decay_skips.len(), 1);
        assert_eq!(decay_skips[0].phase, "decay");

        // Non-existent phase returns empty.
        let empty = db.get_collation_skips("nonexistent").unwrap();
        assert!(empty.is_empty());
    }
}
