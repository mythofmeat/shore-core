//! Markdown memory store — filesystem-based memory entries.
//!
//! Replaces the opaque SQLite `entries` table with inspectable, git-diffable
//! markdown files in `{character}/memories/`.
//!
//! Each file is pure markdown — no YAML frontmatter, no structured metadata.
//! The assistant decides the structure: headings, bullet points, nested folders,
//! filenames. Trust the model to organize.
//!
//! Directory layout:
//! ```text
//! {character}/memories/
//!   README.md              # Character-curated index
//!   topics/
//!     gaming/
//!       doom.md
//!   people/
//!     ren.md
//! ```

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, Utc};
use tokio::fs;
use tracing::info;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum MarkdownStoreError {
    #[error("io: {0}")]
    Io(String),
    #[error("path traversal: {0}")]
    PathTraversal(String),
    #[error("not found: {0}")]
    NotFound(String),
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

/// A memory entry read from the markdown store.
#[derive(Debug, Clone)]
pub struct MarkdownEntry {
    /// Relative path within the memories directory (e.g., "topics/gaming/doom.md").
    pub path: String,
    /// Full markdown content.
    pub content: String,
    /// File size in bytes.
    pub size: u64,
    /// Last modified timestamp (RFC3339).
    pub modified_at: String,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

pub struct MarkdownMemoryStore {
    base_dir: PathBuf,
}

impl MarkdownMemoryStore {
    /// Open (or create) the markdown memory store for a character.
    pub async fn open(base_dir: impl AsRef<Path>) -> Result<Self, MarkdownStoreError> {
        let base = base_dir.as_ref().to_path_buf();
        if !base.exists() {
            fs::create_dir_all(&base)
                .await
                .map_err(|e| MarkdownStoreError::Io(e.to_string()))?;
        }
        Ok(Self { base_dir: base })
    }

    /// Synchronous version of `open` for contexts where async is unavailable.
    pub fn open_sync(base_dir: impl AsRef<Path>) -> Result<Self, MarkdownStoreError> {
        let base = base_dir.as_ref().to_path_buf();
        if !base.exists() {
            std::fs::create_dir_all(&base).map_err(|e| MarkdownStoreError::Io(e.to_string()))?;
        }
        Ok(Self { base_dir: base })
    }

    /// List all `.md` files in the store, recursively.
    pub async fn list_all(&self) -> Result<Vec<MarkdownEntry>, MarkdownStoreError> {
        let mut entries = Vec::new();
        self.collect_md_files(&self.base_dir, &mut entries).await?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }

    /// Read a single entry by relative path.
    pub async fn read(&self, rel_path: &str) -> Result<MarkdownEntry, MarkdownStoreError> {
        let path = self.resolve_path(rel_path)?;
        if !path.exists() {
            return Err(MarkdownStoreError::NotFound(rel_path.to_string()));
        }
        let content = fs::read_to_string(&path)
            .await
            .map_err(|e| MarkdownStoreError::Io(e.to_string()))?;
        let meta = fs::metadata(&path)
            .await
            .map_err(|e| MarkdownStoreError::Io(e.to_string()))?;
        let modified = meta
            .modified()
            .ok()
            .map(format_modified_at)
            .unwrap_or_default();
        Ok(MarkdownEntry {
            path: rel_path.to_string(),
            content,
            size: meta.len(),
            modified_at: modified,
        })
    }

    /// Write (create or overwrite) an entry.
    pub async fn write(&self, rel_path: &str, content: &str) -> Result<(), MarkdownStoreError> {
        let path = self.resolve_path(rel_path)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .await
                .map_err(|e| MarkdownStoreError::Io(e.to_string()))?;
        }
        fs::write(&path, content)
            .await
            .map_err(|e| MarkdownStoreError::Io(e.to_string()))?;
        info!(path = %rel_path, bytes = content.len(), "wrote memory entry");
        Ok(())
    }

    /// Delete an entry.
    pub async fn delete(&self, rel_path: &str) -> Result<(), MarkdownStoreError> {
        let path = self.resolve_path(rel_path)?;
        if !path.exists() {
            return Err(MarkdownStoreError::NotFound(rel_path.to_string()));
        }
        fs::remove_file(&path)
            .await
            .map_err(|e| MarkdownStoreError::Io(e.to_string()))?;
        // Try to clean up empty parent directories.
        if let Some(parent) = path.parent() {
            if parent != self.base_dir {
                let _ = fs::remove_dir(parent).await;
            }
        }
        Ok(())
    }

    /// Simple text search across all entries. Returns entries whose content
    /// contains the query string (case-insensitive).
    pub async fn search_text(&self, query: &str) -> Result<Vec<MarkdownEntry>, MarkdownStoreError> {
        let all = self.list_all().await?;
        let q = query.to_lowercase();
        let mut results = Vec::new();
        for entry in all {
            if entry.content.to_lowercase().contains(&q) {
                results.push(entry);
            }
        }
        Ok(results)
    }

    /// Migrate existing SQLite entries to markdown files.
    ///
    /// Writes each entry to `migrated/{id}.md` with the summary_text as body.
    pub async fn migrate_from_entries(
        &self,
        entries: &[crate::memory::db::Entry],
    ) -> Result<usize, MarkdownStoreError> {
        let mut count = 0;
        for entry in entries {
            let filename = if entry.topic_key.is_empty() {
                format!("migrated/{}.md", entry.id)
            } else {
                format!(
                    "migrated/{}_{}.md",
                    sanitize_filename(&entry.topic_key),
                    entry.id
                )
            };
            let content = format!("# {}\n\n{}\n", entry.topic_key, entry.summary_text);
            self.write(&filename, &content).await?;
            count += 1;
        }
        info!(count, "migrated entries to markdown");
        Ok(count)
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    async fn collect_md_files(
        &self,
        dir: &Path,
        entries: &mut Vec<MarkdownEntry>,
    ) -> Result<(), MarkdownStoreError> {
        let mut read_dir = fs::read_dir(dir)
            .await
            .map_err(|e| MarkdownStoreError::Io(e.to_string()))?;

        while let Some(child) = read_dir
            .next_entry()
            .await
            .map_err(|e| MarkdownStoreError::Io(e.to_string()))?
        {
            let path = child.path();
            let meta = child
                .metadata()
                .await
                .map_err(|e| MarkdownStoreError::Io(e.to_string()))?;
            if meta.is_dir() {
                Box::pin(self.collect_md_files(&path, entries)).await?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                let rel = path
                    .strip_prefix(&self.base_dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                let content = fs::read_to_string(&path)
                    .await
                    .map_err(|e| MarkdownStoreError::Io(e.to_string()))?;
                let modified = meta
                    .modified()
                    .ok()
                    .map(format_modified_at)
                    .unwrap_or_default();
                entries.push(MarkdownEntry {
                    path: rel,
                    content,
                    size: meta.len(),
                    modified_at: modified,
                });
            }
        }
        Ok(())
    }

    fn resolve_path(&self, rel_path: &str) -> Result<PathBuf, MarkdownStoreError> {
        let rel = rel_path.trim();
        if rel.is_empty() {
            return Err(MarkdownStoreError::PathTraversal("empty path".into()));
        }
        for component in Path::new(rel).components() {
            match component {
                std::path::Component::ParentDir => {
                    return Err(MarkdownStoreError::PathTraversal(
                        "path traversal (..) not allowed".into(),
                    ));
                }
                std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                    return Err(MarkdownStoreError::PathTraversal(
                        "absolute paths not allowed".into(),
                    ));
                }
                _ => {}
            }
        }
        Ok(self.base_dir.join(rel))
    }
}

fn format_modified_at(time: std::time::SystemTime) -> String {
    let utc: DateTime<Utc> = time.into();
    utc.with_timezone(&Local).to_rfc3339()
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            ' ' | '/' | '\\' | ':' => '-',
            _ => '-',
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_creates_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();
        assert!(store.base_dir.exists());
    }

    #[tokio::test]
    async fn write_read_cycle() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        store
            .write("topics/gaming/doom.md", "# Doom\n\n- UV-Max speedrunner\n")
            .await
            .unwrap();

        let entry = store.read("topics/gaming/doom.md").await.unwrap();
        assert_eq!(entry.content, "# Doom\n\n- UV-Max speedrunner\n");
        assert_eq!(entry.path, "topics/gaming/doom.md");
    }

    #[tokio::test]
    async fn read_reports_real_modified_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        store.write("a.md", "A").await.unwrap();

        let path = tmp.path().join("memories/a.md");
        let metadata = std::fs::metadata(&path).unwrap();
        let expected = format_modified_at(metadata.modified().unwrap());

        let entry = store.read("a.md").await.unwrap();
        assert_eq!(entry.modified_at, expected);
    }

    #[tokio::test]
    async fn list_all_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        store.write("a.md", "A").await.unwrap();
        store.write("deep/b.md", "B").await.unwrap();

        let entries = store.list_all().await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, "a.md");
        assert_eq!(entries[1].path, "deep/b.md");
    }

    #[tokio::test]
    async fn search_text_finds_matches() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        store.write("a.md", "Ren likes chocolate").await.unwrap();
        store.write("b.md", "Alice prefers tea").await.unwrap();

        let results = store.search_text("chocolate").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "a.md");
    }

    #[tokio::test]
    async fn delete_removes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        store.write("temp.md", "temp").await.unwrap();
        store.delete("temp.md").await.unwrap();

        assert!(store.read("temp.md").await.is_err());
    }

    #[tokio::test]
    async fn rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MarkdownMemoryStore::open(tmp.path().join("memories"))
            .await
            .unwrap();

        assert!(store.read("../secret.md").await.is_err());
        assert!(store.write("../secret.md", "x").await.is_err());
    }
}
