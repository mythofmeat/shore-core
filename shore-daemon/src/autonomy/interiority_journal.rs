//! Rolling journal of autonomous interiority activity.
//!
//! Persists thoughts, tool calls, tool results, and sent messages as JSONL.
//! Read at the start of each interiority tick and rendered as human-readable
//! text in the prompt so the character has continuity across ticks.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub ts: String,
    #[serde(rename = "type")]
    pub entry_type: JournalEntryType,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalEntryType {
    Thought,
    ToolCall,
    ToolResult,
    MessageSent,
}

// ---------------------------------------------------------------------------
// Path helper
// ---------------------------------------------------------------------------

pub fn journal_path(data_dir: &Path, character: &str) -> PathBuf {
    data_dir.join(character).join("interiority_journal.jsonl")
}

// ---------------------------------------------------------------------------
// File I/O
// ---------------------------------------------------------------------------

/// Read all journal entries from disk. Returns empty vec if the file doesn't
/// exist. Malformed lines are skipped with a warning.
pub fn read_journal(path: &Path) -> Vec<JournalEntry> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "Failed to open interiority journal");
            return Vec::new();
        }
    };

    let reader = io::BufReader::new(file);
    let mut entries = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                warn!(path = %path.display(), line = i + 1, error = %e, "Journal read error");
                continue;
            }
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<JournalEntry>(trimmed) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                warn!(path = %path.display(), line = i + 1, error = %e, "Skipping malformed journal entry");
            }
        }
    }

    entries
}

/// Append entries to the journal file. Creates parent directories if needed.
pub fn append_entries(path: &Path, entries: &[JournalEntry]) -> io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    for entry in entries {
        let json = serde_json::to_string(entry)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        writeln!(file, "{json}")?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render journal entries as human-readable text for the interiority prompt.
pub fn render_journal(entries: &[JournalEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        out.push('[');
        out.push_str(&entry.ts);
        out.push_str("] ");
        match entry.entry_type {
            JournalEntryType::Thought => {
                out.push_str("thought: ");
                out.push_str(&entry.content);
            }
            JournalEntryType::ToolCall => {
                out.push_str("tool_call: ");
                out.push_str(&entry.content);
            }
            JournalEntryType::ToolResult => {
                out.push_str("→ ");
                out.push_str(&entry.content);
            }
            JournalEntryType::MessageSent => {
                out.push_str("you sent: ");
                out.push_str(&entry.content);
            }
        }
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Truncation & compaction
// ---------------------------------------------------------------------------

/// Default character budget (~4096 tokens at ~4 chars/token).
pub const DEFAULT_BUDGET_CHARS: usize = 16_000;

/// Return the newest entries that fit within `max_chars` when rendered.
/// Drops oldest entries first. Returned in chronological order.
pub fn truncate_to_budget(entries: &[JournalEntry], max_chars: usize) -> Vec<JournalEntry> {
    if entries.is_empty() {
        return Vec::new();
    }

    // Render from newest to oldest, accumulating size.
    let mut kept_start = entries.len();
    let mut total_chars = 0usize;

    for i in (0..entries.len()).rev() {
        let rendered_len = rendered_entry_len(&entries[i]);
        if total_chars + rendered_len > max_chars {
            break;
        }
        total_chars += rendered_len;
        kept_start = i;
    }

    entries[kept_start..].to_vec()
}

/// Rewrite the journal file keeping only entries that fit the budget.
/// Uses write-to-tmp + rename for atomicity.
pub fn compact_file(path: &Path, max_chars: usize) -> io::Result<()> {
    let entries = read_journal(path);
    let kept = truncate_to_budget(&entries, max_chars);

    let tmp_path = path.with_extension("jsonl.tmp");
    {
        let mut file = fs::File::create(&tmp_path)?;
        for entry in &kept {
            let json = serde_json::to_string(entry)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            writeln!(file, "{json}")?;
        }
    }
    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Estimate the rendered character length of a single entry (for budget calc).
fn rendered_entry_len(entry: &JournalEntry) -> usize {
    // "[ts] prefix: content\n"
    let prefix_len = match entry.entry_type {
        JournalEntryType::Thought => 10,     // "thought: "
        JournalEntryType::ToolCall => 12,    // "tool_call: "
        JournalEntryType::ToolResult => 4,   // "→ "  (3 bytes UTF-8 + space)
        JournalEntryType::MessageSent => 11, // "you sent: "
    };
    // "[" + ts + "] " + prefix + content + "\n"
    1 + entry.ts.len() + 2 + prefix_len + entry.content.len() + 1
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn entry(ty: JournalEntryType, content: &str) -> JournalEntry {
        JournalEntry {
            ts: "2026-04-03T12:00:00Z".to_string(),
            entry_type: ty,
            content: content.to_string(),
        }
    }

    #[test]
    fn read_empty_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent.jsonl");
        assert!(read_journal(&path).is_empty());
    }

    #[test]
    fn write_and_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("char").join("journal.jsonl");

        let entries = vec![
            entry(JournalEntryType::Thought, "thinking..."),
            entry(JournalEntryType::ToolCall, "scratchpad_list({})"),
            entry(JournalEntryType::ToolResult, "[\"notes.md\"]"),
        ];

        append_entries(&path, &entries).unwrap();
        let read = read_journal(&path);
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].entry_type, JournalEntryType::Thought);
        assert_eq!(read[0].content, "thinking...");
        assert_eq!(read[2].entry_type, JournalEntryType::ToolResult);
    }

    #[test]
    fn render_journal_formatting() {
        let entries = vec![
            entry(JournalEntryType::Thought, "I should check my notes"),
            entry(JournalEntryType::ToolCall, "scratchpad_read({\"file\":\"notes.md\"})"),
            entry(JournalEntryType::ToolResult, "hello world"),
            entry(JournalEntryType::MessageSent, "Hey there!"),
        ];
        let rendered = render_journal(&entries);
        assert!(rendered.contains("thought: I should check my notes"));
        assert!(rendered.contains("tool_call: scratchpad_read"));
        assert!(rendered.contains("→ hello world"));
        assert!(rendered.contains("you sent: Hey there!"));
    }

    #[test]
    fn truncate_drops_oldest() {
        // Create entries where each renders to a known size.
        let entries: Vec<_> = (0..10)
            .map(|i| entry(JournalEntryType::Thought, &format!("entry number {i}")))
            .collect();

        let full_rendered = render_journal(&entries);
        let half_budget = full_rendered.len() / 2;

        let kept = truncate_to_budget(&entries, half_budget);
        assert!(kept.len() < entries.len());
        assert!(kept.len() > 0);
        // Last entry should always be preserved (newest).
        assert_eq!(kept.last().unwrap().content, "entry number 9");
        // First entry should be dropped (oldest).
        assert_ne!(kept.first().unwrap().content, "entry number 0");
    }

    #[test]
    fn truncate_preserves_order() {
        let entries: Vec<_> = (0..5)
            .map(|i| JournalEntry {
                ts: format!("2026-04-03T12:0{i}:00Z"),
                entry_type: JournalEntryType::Thought,
                content: format!("entry {i}"),
            })
            .collect();

        let kept = truncate_to_budget(&entries, 100_000);
        // All should fit.
        assert_eq!(kept.len(), 5);
        // Chronological order preserved.
        assert!(kept[0].ts < kept[4].ts);
    }

    #[test]
    fn compact_journal_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("journal.jsonl");

        // Write many entries.
        let entries: Vec<_> = (0..100)
            .map(|i| entry(JournalEntryType::Thought, &format!("entry {i} with some padding text to make it longer")))
            .collect();
        append_entries(&path, &entries).unwrap();

        let full_rendered = render_journal(&entries);
        let half_budget = full_rendered.len() / 2;

        compact_file(&path, half_budget).unwrap();

        let after = read_journal(&path);
        assert!(after.len() < entries.len());
        assert!(after.len() > 0);
        // Newest entries preserved.
        assert_eq!(after.last().unwrap().content, entries.last().unwrap().content);
    }

    #[test]
    fn malformed_lines_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("journal.jsonl");

        let mut file = fs::File::create(&path).unwrap();
        writeln!(file, r#"{{"ts":"t1","type":"thought","content":"good"}}"#).unwrap();
        writeln!(file, "this is not json").unwrap();
        writeln!(file, r#"{{"ts":"t2","type":"tool_call","content":"also good"}}"#).unwrap();

        let entries = read_journal(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].content, "good");
        assert_eq!(entries[1].content, "also good");
    }
}
