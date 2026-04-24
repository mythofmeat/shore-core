use std::cmp::Reverse;

use crate::memory::markdown_store::{MarkdownEntry, MarkdownMemoryStore, MarkdownStoreError};

const MAX_DIRECT_HITS: usize = 10;

pub struct MemoryStatus {
    pub total_files: usize,
    pub topic_files: usize,
    pub daily_files: usize,
    pub image_files: usize,
}

pub async fn memory_status(
    store: &MarkdownMemoryStore,
) -> Result<MemoryStatus, MarkdownStoreError> {
    let files = store.list_all().await?;
    let mut status = MemoryStatus {
        total_files: 0,
        topic_files: 0,
        daily_files: 0,
        image_files: 0,
    };

    for entry in files {
        if entry.path == "DREAMS.md" {
            continue;
        }

        status.total_files += 1;
        if entry.path.starts_with("daily/") {
            status.daily_files += 1;
        } else if entry.path.starts_with("images/") {
            status.image_files += 1;
        } else {
            status.topic_files += 1;
        }
    }

    Ok(status)
}

pub fn format_direct_response(query: &str, hits: &[MarkdownEntry]) -> String {
    if hits.is_empty() {
        return format!("No memory files matched '{query}'.");
    }

    let mut lines = vec![format!("Top memory matches for '{query}':")];
    for entry in hits.iter().take(MAX_DIRECT_HITS) {
        let excerpt = excerpt_for_query(&entry.content, query, 220);
        lines.push(format!("- {}\n  {}", entry.path, excerpt));
    }
    lines.join("\n")
}

pub async fn append_daily_note(
    store: &MarkdownMemoryStore,
    timestamp: chrono::DateTime<chrono::FixedOffset>,
    heading: &str,
    body: &str,
) -> Result<String, MarkdownStoreError> {
    let date = timestamp.format("%Y-%m-%d").to_string();
    let time = timestamp.format("%H:%M").to_string();
    let path = format!("daily/{date}.md");
    let existing = match store.read(&path).await {
        Ok(entry) => entry.content,
        Err(MarkdownStoreError::NotFound(_)) => {
            format!("# Daily Notes for {date}\n")
        }
        Err(e) => return Err(e),
    };

    let mut updated = existing.trim_end().to_string();
    if !updated.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(&format!("## {time} - {heading}\n\n{}\n", body.trim()));

    store.write(&path, &updated).await?;
    Ok(path)
}

pub async fn append_dream_entry(
    store: &MarkdownMemoryStore,
    timestamp: chrono::DateTime<chrono::FixedOffset>,
    title: &str,
    body: &str,
) -> Result<(), MarkdownStoreError> {
    let path = "DREAMS.md";
    let existing = match store.read(path).await {
        Ok(entry) => entry.content,
        Err(MarkdownStoreError::NotFound(_)) => "# Dreams\n".to_string(),
        Err(e) => return Err(e),
    };

    let mut updated = existing.trim_end().to_string();
    if !updated.is_empty() {
        updated.push_str("\n\n");
    }
    updated.push_str(&format!(
        "## {} - {}\n\n{}\n",
        timestamp.format("%Y-%m-%d %H:%M"),
        title,
        body.trim()
    ));

    store.write(path, &updated).await
}

pub async fn recent_dream_entries(
    store: &MarkdownMemoryStore,
    limit: usize,
) -> Result<Vec<String>, MarkdownStoreError> {
    let content = match store.read("DREAMS.md").await {
        Ok(entry) => entry.content,
        Err(MarkdownStoreError::NotFound(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut sections = content
        .split("\n## ")
        .filter_map(|section| {
            let trimmed = section.trim();
            if trimmed.is_empty() || trimmed == "# Dreams" {
                None
            } else if let Some(rest) = trimmed.strip_prefix("# Dreams") {
                let entry = rest.trim();
                if entry.is_empty() {
                    None
                } else {
                    Some(format!("## {entry}"))
                }
            } else {
                Some(format!("## {trimmed}"))
            }
        })
        .collect::<Vec<_>>();
    sections.sort_by_key(|entry| Reverse(entry.clone()));
    sections.truncate(limit);
    Ok(sections)
}

pub async fn recent_daily_notes(
    store: &MarkdownMemoryStore,
    limit: usize,
) -> Result<Vec<MarkdownEntry>, MarkdownStoreError> {
    let mut notes = store
        .list_all()
        .await?
        .into_iter()
        .filter(|entry| entry.path.starts_with("daily/"))
        .collect::<Vec<_>>();
    notes.sort_by_key(|entry| Reverse(entry.path.clone()));
    notes.truncate(limit);
    Ok(notes)
}

pub fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

pub fn excerpt_for_query(text: &str, query: &str, limit: usize) -> String {
    let normalized_query = query.trim().to_lowercase();
    if normalized_query.is_empty() {
        return excerpt(text, limit);
    }

    let terms = normalized_query
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|term| term.len() >= 2)
        .collect::<Vec<_>>();
    let lines = text.lines().collect::<Vec<_>>();

    for idx in 0..lines.len() {
        let line = lines[idx].trim();
        if line.is_empty() {
            continue;
        }

        let lower = line.to_lowercase();
        let matched =
            lower.contains(&normalized_query) || terms.iter().any(|term| lower.contains(term));
        if !matched {
            continue;
        }

        let start = idx.saturating_sub(1);
        let end = (idx + 2).min(lines.len());
        let window = lines[start..end]
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");

        if !window.is_empty() {
            return excerpt(&window, limit);
        }
    }

    excerpt(text, limit)
}

fn excerpt(text: &str, limit: usize) -> String {
    let normalized = text.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    if normalized.chars().count() > limit {
        format!("{}...", truncate_chars(&normalized, limit))
    } else {
        normalized
    }
}

#[cfg(test)]
mod tests {
    use super::excerpt_for_query;

    #[test]
    fn excerpt_for_query_prefers_matching_line_context() {
        let text = "\
Intro that should not be surfaced first.

## Preferences
Ren likes lapsang souchong tea after long walks.

Trailing detail.";

        let excerpt = excerpt_for_query(text, "lapsang souchong", 120);
        assert!(excerpt.contains("lapsang souchong tea"));
        assert!(!excerpt.starts_with("Intro that should not be surfaced first."));
    }
}
