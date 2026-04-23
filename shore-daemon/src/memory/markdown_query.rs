use std::cmp::Reverse;

use serde_json::json;
use shore_config::models::ResolvedModel;

use crate::memory::agent_llm::{AgentLlm, AgentLlmError};
use crate::memory::markdown_store::{MarkdownEntry, MarkdownMemoryStore, MarkdownStoreError};

const MAX_QUERY_FILES: usize = 8;
const MAX_QUERY_CHARS_PER_FILE: usize = 4_000;
const MAX_DIRECT_HITS: usize = 10;

#[derive(Debug, thiserror::Error)]
pub enum MarkdownQueryError {
    #[error("markdown store: {0}")]
    Store(String),
    #[error("llm: {0}")]
    Llm(String),
}

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
        let excerpt = excerpt(&entry.content, 220);
        lines.push(format!("- {}\n  {}", entry.path, excerpt));
    }
    lines.join("\n")
}

pub async fn answer_query(
    request: &str,
    character_name: &str,
    user_name: &str,
    store: &MarkdownMemoryStore,
    llm: &dyn AgentLlm,
    model: &ResolvedModel,
) -> Result<String, MarkdownQueryError> {
    let hits = store
        .search_text(request)
        .await
        .map_err(|e| MarkdownQueryError::Store(e.to_string()))?;

    if hits.is_empty() {
        return Ok("I couldn't find any relevant memory files for that.".to_string());
    }

    let selected = hits.into_iter().take(MAX_QUERY_FILES).collect::<Vec<_>>();
    let memory_dump = selected
        .iter()
        .map(|entry| {
            format!(
                "<memory_file path=\"{}\" modified_at=\"{}\">\n{}\n</memory_file>",
                entry.path,
                entry.modified_at,
                truncate_chars(&entry.content, MAX_QUERY_CHARS_PER_FILE)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let system = format!(
        "You are helping {character_name} answer a memory question about {user_name}. \
Use only the provided markdown memory files. If the files do not support a claim, \
say you couldn't find it. Do not invent facts. Prefer a short direct answer."
    );
    let user = format!("Question:\n{request}\n\nRelevant memory files:\n{memory_dump}");

    let response = llm
        .generate(
            vec![json!({"role": "user", "content": user})],
            Some(json!(system)),
            None,
            model,
        )
        .await
        .map_err(map_llm_error)?;

    Ok(response.text.trim().to_string())
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

fn excerpt(text: &str, limit: usize) -> String {
    let normalized = text.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    if normalized.chars().count() > limit {
        format!("{}...", truncate_chars(&normalized, limit))
    } else {
        normalized
    }
}

fn map_llm_error(error: AgentLlmError) -> MarkdownQueryError {
    MarkdownQueryError::Llm(error.to_string())
}
