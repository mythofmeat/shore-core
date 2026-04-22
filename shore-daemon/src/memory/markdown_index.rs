use std::path::Path;

use chrono::{DateTime, FixedOffset, Local};
use sha2::{Digest, Sha256};

use crate::memory::agent::AgentIndexer;
use crate::memory::db::{Entry, MemoryDB};
use crate::memory::markdown_store::MarkdownMemoryStore;

const MARKDOWN_ENTRY_SOURCE: &str = "markdown_store";
const MARKDOWN_ENTRY_TYPE: &str = "markdown_file";

pub async fn sync_store_changes(
    db: &MemoryDB,
    store: &MarkdownMemoryStore,
    indexer: Option<&dyn AgentIndexer>,
    reason: &str,
) -> Result<usize, String> {
    let entries = store.list_all().await.map_err(|e| e.to_string())?;
    let mut synced = 0usize;

    for entry in entries {
        let existing = db
            .get_entry_by_file_path(&entry.path)
            .map_err(|e| e.to_string())?;
        if !needs_sync(existing.as_ref(), &entry.modified_at) {
            continue;
        }

        let (shadow, _) = upsert_markdown_entry(db, &entry.path, &entry.content, reason)
            .map_err(|e| e.to_string())?;
        if let Some(indexer) = indexer {
            indexer
                .index_entry(&shadow.id, &shadow.summary_text)
                .await
                .map_err(|e| e.to_string())?;
        }
        synced += 1;
    }

    Ok(synced)
}

pub async fn sync_markdown_entry_content(
    db: &MemoryDB,
    indexer: Option<&dyn AgentIndexer>,
    rel_path: &str,
    content: &str,
    reason: &str,
) -> Result<String, String> {
    let (shadow, _) =
        upsert_markdown_entry(db, rel_path, content, reason).map_err(|e| e.to_string())?;
    if let Some(indexer) = indexer {
        indexer
            .index_entry(&shadow.id, &shadow.summary_text)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(shadow.id)
}

pub fn upsert_markdown_entry(
    db: &MemoryDB,
    rel_path: &str,
    content: &str,
    reason: &str,
) -> rusqlite::Result<(Entry, bool)> {
    let existing = db.get_entry_by_file_path(rel_path)?;
    let now = Local::now().to_rfc3339();
    let topic_key = topic_key_from_path(rel_path);
    let topic_tags = topic_tags_from_path(rel_path);

    let entry = if let Some(existing) = existing {
        Entry {
            id: existing.id,
            memory_type: existing.memory_type,
            source: MARKDOWN_ENTRY_SOURCE.to_string(),
            reason: reason.to_string(),
            status: existing.status,
            confidence: existing.confidence,
            summary_text: content.to_string(),
            topic_tags,
            topic_key,
            start_timestamp: existing.start_timestamp,
            end_timestamp: existing.end_timestamp,
            message_count: existing.message_count,
            source_entry_ids: existing.source_entry_ids,
            related_entry_ids: existing.related_entry_ids,
            superseded_by: existing.superseded_by,
            created_at: existing.created_at,
            updated_at: now,
            entry_type: MARKDOWN_ENTRY_TYPE.to_string(),
            image_path: existing.image_path,
            collated_at: existing.collated_at,
            file_path: rel_path.to_string(),
        }
    } else {
        Entry {
            id: entry_id_for_path(rel_path),
            memory_type: "semantic".to_string(),
            source: MARKDOWN_ENTRY_SOURCE.to_string(),
            reason: reason.to_string(),
            status: "active".to_string(),
            confidence: 0.9,
            summary_text: content.to_string(),
            topic_tags,
            topic_key,
            start_timestamp: now.clone(),
            end_timestamp: now.clone(),
            message_count: 0,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: now.clone(),
            updated_at: now,
            entry_type: MARKDOWN_ENTRY_TYPE.to_string(),
            image_path: String::new(),
            collated_at: String::new(),
            file_path: rel_path.to_string(),
        }
    };

    let created = db.get_entry(&entry.id)?.is_none();
    if created {
        db.create_entry(&entry)?;
    } else {
        db.update_entry(&entry)?;
    }

    Ok((entry, created))
}

pub fn entry_id_for_path(rel_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(rel_path.as_bytes());
    let digest = hasher.finalize();
    format!("md_{:x}", digest)[..19].to_string()
}

fn topic_key_from_path(rel_path: &str) -> String {
    Path::new(rel_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string()
}

fn topic_tags_from_path(rel_path: &str) -> String {
    let mut tags = Vec::new();
    for component in Path::new(rel_path).components() {
        let value = component.as_os_str().to_string_lossy();
        if value.is_empty() {
            continue;
        }
        if let Some(stripped) = value.strip_suffix(".md") {
            if !stripped.is_empty() {
                tags.push(stripped.to_string());
            }
        } else {
            tags.push(value.to_string());
        }
    }
    tags.join(",")
}

fn needs_sync(existing: Option<&Entry>, modified_at: &str) -> bool {
    let Some(existing) = existing else {
        return true;
    };
    let Some(file_modified) = parse_timestamp(modified_at) else {
        return true;
    };
    let Some(entry_updated) = parse_timestamp(&existing.updated_at) else {
        return true;
    };
    file_modified > entry_updated
}

fn parse_timestamp(value: &str) -> Option<DateTime<FixedOffset>> {
    if value.trim().is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_id_is_stable_for_path() {
        assert_eq!(
            entry_id_for_path("people/ren.md"),
            entry_id_for_path("people/ren.md")
        );
        assert_ne!(
            entry_id_for_path("people/ren.md"),
            entry_id_for_path("people/sam.md")
        );
    }

    #[test]
    fn topic_tags_follow_path_components() {
        assert_eq!(topic_key_from_path("topics/gaming/doom.md"), "doom");
        assert_eq!(
            topic_tags_from_path("topics/gaming/doom.md"),
            "topics,gaming,doom"
        );
    }
}
