use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// A single first-person recap entry written by the character during an
/// interiority tick. Persisted as one JSON line in `recaps.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecapEntry {
    pub timestamp: DateTime<FixedOffset>,
    pub tick_id: String,
    pub recap: String,
}

/// Append-only store backed by a JSONL sidecar file.
///
/// Loaded from disk on demand — not held in long-lived shared state.
/// Writes append a single line; reads load the full file.
#[derive(Debug)]
pub struct RecapStore {
    path: PathBuf,
    entries: Vec<RecapEntry>,
}

impl RecapStore {
    /// Load all recap entries from `path`. Missing file → empty store.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = Self::read_entries(&path);
        Self { path, entries }
    }

    /// Append one entry: push to memory and append a JSONL line to disk.
    pub fn append(&mut self, entry: RecapEntry) -> std::io::Result<()> {
        let line = serde_json::to_string(&entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{line}")?;

        self.entries.push(entry);
        Ok(())
    }

    /// Entries whose timestamp falls strictly between `from` and `to`.
    pub fn entries_in_range(
        &self,
        from: &DateTime<FixedOffset>,
        to: &DateTime<FixedOffset>,
    ) -> Vec<&RecapEntry> {
        self.entries
            .iter()
            .filter(|e| &e.timestamp > from && &e.timestamp < to)
            .collect()
    }

    /// All entries in append order.
    pub fn entries(&self) -> &[RecapEntry] {
        &self.entries
    }

    // -- internal -------------------------------------------------------------

    fn read_entries(path: &Path) -> Vec<RecapEntry> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return Vec::new(), // missing file is fine
        };

        let mut entries = Vec::new();
        for (i, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<RecapEntry>(line) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    warn!(line_num = i + 1, error = %e, "recaps.jsonl: skipping malformed line");
                }
            }
        }
        entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn ts(hour: u32) -> DateTime<FixedOffset> {
        let offset = FixedOffset::west_opt(7 * 3600).unwrap();
        offset
            .with_ymd_and_hms(2026, 4, 7, hour, 0, 0)
            .single()
            .unwrap()
    }

    fn entry(hour: u32, text: &str) -> RecapEntry {
        RecapEntry {
            timestamp: ts(hour),
            tick_id: format!("tick_{hour}"),
            recap: text.to_string(),
        }
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = TempDir::new().unwrap();
        let store = RecapStore::load(dir.path().join("recaps.jsonl"));
        assert!(store.entries().is_empty());
    }

    #[test]
    fn append_and_reload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("recaps.jsonl");

        let mut store = RecapStore::load(&path);
        store.append(entry(10, "first")).unwrap();
        store.append(entry(14, "second")).unwrap();
        assert_eq!(store.entries().len(), 2);

        // Reload from disk.
        let store2 = RecapStore::load(&path);
        assert_eq!(store2.entries().len(), 2);
        assert_eq!(store2.entries()[0].recap, "first");
        assert_eq!(store2.entries()[1].recap, "second");
    }

    #[test]
    fn entries_in_range_filters_strictly() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("recaps.jsonl");

        let mut store = RecapStore::load(&path);
        store.append(entry(8, "early")).unwrap();
        store.append(entry(10, "mid")).unwrap();
        store.append(entry(12, "late")).unwrap();

        // Range (9, 11) should only include hour=10.
        let range = store.entries_in_range(&ts(9), &ts(11));
        assert_eq!(range.len(), 1);
        assert_eq!(range[0].recap, "mid");

        // Boundary: exact match excluded (strictly between).
        let exact = store.entries_in_range(&ts(10), &ts(12));
        assert!(exact.is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("recaps.jsonl");

        let mut store = RecapStore::load(&path);
        store.append(entry(10, "good")).unwrap();
        // Corrupt the file with a bad line.
        std::fs::write(
            &path,
            format!(
                "{}\n{{\n",
                serde_json::to_string(&entry(10, "good")).unwrap()
            ),
        )
        .unwrap();

        let store2 = RecapStore::load(&path);
        assert_eq!(store2.entries().len(), 1);
    }
}
