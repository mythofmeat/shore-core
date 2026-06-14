//! Full-fidelity transcript log for background LLM calls.
//!
//! Heartbeat ticks and the dreaming/librarian pass run their own LLM calls with
//! tools, outside any user-facing conversation. The ring-buffer event log
//! (`heartbeat.jsonl`) and the dreams audit log (`DREAMS.md`) only keep
//! truncated summaries, and the tracing stream truncates reasoning and tool I/O
//! to 200 chars before emitting. Neither lets you reconstruct exactly what a
//! background call thought, which tools it ran, what those tools returned, or
//! which model/provider actually served the request.
//!
//! This module records that — one [`TranscriptEntry`] per LLM call — to a
//! rolling JSONL file in the character's data directory, kept separate per
//! [`TranscriptSource`] (heartbeat / dreaming). Retention is **time-based**:
//! entries older than [`TRANSCRIPT_RETENTION_DAYS`] are pruned on each append.
//! Tool input and output are stored **in full, untruncated** — these are meant
//! to be complete, inspectable, debuggable logs. The file lives alongside
//! `heartbeat.jsonl` and `DREAMS.md`, never inside the workspace, so it never
//! bleeds into prompts or memory snapshots.

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use shore_config::character_data_dir;
use shore_llm::types::GenerateResponse;
use shore_protocol::types::ContentBlock;
use tracing::warn;

/// How long a transcript entry is retained. On each append, entries whose
/// timestamp is older than this window are pruned. Bytes per entry are
/// unbounded (full tool I/O is kept), so retention is time-based rather than a
/// count or size cap.
const TRANSCRIPT_RETENTION_DAYS: i64 = 14;

/// Initial allocation hint for the in-memory entry list. Not a retention cap —
/// the list grows as needed and is bounded only by [`TRANSCRIPT_RETENTION_DAYS`].
const TRANSCRIPT_ALLOC_HINT: usize = 256;

/// Which background subsystem produced a transcript entry. Each source has its
/// own file so their ring buffers do not contend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptSource {
    /// An autonomy heartbeat tick.
    Heartbeat,
    /// A dreaming / memory-librarian pass.
    Dreaming,
}

impl TranscriptSource {
    /// File name for this source's transcript log.
    fn file_name(self) -> &'static str {
        match self {
            Self::Heartbeat => "heartbeat-transcript.jsonl",
            Self::Dreaming => "dreaming-transcript.jsonl",
        }
    }

    /// Stable wire/CLI label for this source.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Heartbeat => "heartbeat",
            Self::Dreaming => "dreaming",
        }
    }

    /// Parse a source from its wire/CLI label.
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "heartbeat" => Some(Self::Heartbeat),
            "dreaming" => Some(Self::Dreaming),
            _ => None,
        }
    }
}

/// Token usage for a single background LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
}

/// One tool invocation made during a background call, with full (un-truncated)
/// input and output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptToolCall {
    pub name: String,
    pub input: serde_json::Value,
    pub output: String,
    pub is_error: bool,
}

/// A single background LLM call: its reasoning, tool calls, visible text, and
/// the model/provider that actually served it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptEntry {
    /// RFC3339 timestamp of when the call completed.
    pub timestamp: String,
    /// Which subsystem produced the call.
    pub source: TranscriptSource,
    /// Ledger call type (e.g. `heartbeat`, `heartbeat_tool_loop`, `dreaming`).
    pub call_type: String,
    /// Zero-based iteration within the tool loop for this tick/pass.
    pub iteration: u32,
    /// Model the provider reported serving the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider key the request ran against (e.g. `anthropic`, `deepseek`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Provider-reported finish reason for the call.
    pub finish_reason: String,
    /// Token usage for the call.
    pub usage: TranscriptUsage,
    /// Full reasoning/thinking text emitted by the call (redacted thinking is
    /// recorded as a placeholder marker).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning: Vec<String>,
    /// Full visible (non-thinking) assistant text for the call.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Tool calls dispatched from the call, with their results.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<TranscriptToolCall>,
}

impl TranscriptEntry {
    /// Build an entry from a completed response. Reasoning and visible text are
    /// extracted from the response's content blocks; tool calls are supplied by
    /// the caller (the dispatch layer holds the un-truncated results).
    pub fn from_response(
        source: TranscriptSource,
        call_type: &str,
        iteration: u32,
        timestamp: String,
        provider: Option<String>,
        resp: &GenerateResponse,
        tool_calls: Vec<TranscriptToolCall>,
    ) -> Self {
        let mut reasoning = Vec::new();
        let mut text = String::new();
        for block in &resp.content_blocks {
            match block {
                ContentBlock::Thinking { thinking, .. } => {
                    if !thinking.trim().is_empty() {
                        reasoning.push(thinking.clone());
                    }
                }
                ContentBlock::RedactedThinking { .. } => {
                    reasoning.push("[redacted thinking]".to_owned());
                }
                ContentBlock::Text { text: t } => {
                    if !t.is_empty() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
                ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. } => {}
            }
        }
        Self {
            timestamp,
            source,
            call_type: call_type.to_owned(),
            iteration,
            model: (!resp.model.is_empty()).then(|| resp.model.clone()),
            provider,
            finish_reason: resp.finish_reason.clone(),
            usage: TranscriptUsage {
                input_tokens: resp.usage.input_tokens,
                output_tokens: resp.usage.output_tokens,
                cache_read_tokens: resp.usage.cache_read_tokens,
            },
            reasoning,
            text,
            tool_calls,
        }
    }
}

/// Canonical path of a source's transcript log for a character.
pub fn transcript_log_path(data_dir: &Path, character: &str, source: TranscriptSource) -> PathBuf {
    character_data_dir(data_dir, character).join(source.file_name())
}

/// Append `entries` to the source's transcript log, then prune anything older
/// than [`TRANSCRIPT_RETENTION_DAYS`]. Loads the existing file, appends, drops
/// out-of-window entries, and rewrites atomically (tmp + rename). A no-op when
/// `entries` is empty. Logs a warning and returns `Err` on I/O failure; callers
/// treat transcript writes as best-effort.
pub fn append_entries(
    data_dir: &Path,
    character: &str,
    source: TranscriptSource,
    entries: &[TranscriptEntry],
) -> io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let path = transcript_log_path(data_dir, character, source);
    let mut all = load_entries(&path);
    all.extend(entries.iter().cloned());
    // Prune anything outside the retention window. `checked_sub_signed` only
    // returns `None` near the representable date floor (never for "now"); skip
    // pruning rather than risk dropping everything in that impossible case.
    if let Some(cutoff) = Utc::now().checked_sub_signed(Duration::days(TRANSCRIPT_RETENTION_DAYS)) {
        retain_within(&mut all, cutoff);
    }
    write_atomic(&path, &all)
}

/// Read the most recent `limit` entries (newest first) from a source's
/// transcript log, or an empty vector when the file does not exist or cannot be
/// read. `limit` is a display bound only — independent of the retention window.
pub fn read_recent(
    data_dir: &Path,
    character: &str,
    source: TranscriptSource,
    limit: usize,
) -> Vec<TranscriptEntry> {
    let path = transcript_log_path(data_dir, character, source);
    let mut entries = load_entries(&path);
    entries.reverse();
    entries.truncate(limit);
    entries
}

/// Drop entries whose timestamp is older than `cutoff`. Entries with an
/// unparseable timestamp are kept — a bad timestamp must never silently delete a
/// log line.
fn retain_within(entries: &mut Vec<TranscriptEntry>, cutoff: DateTime<Utc>) {
    entries.retain(|e| match DateTime::parse_from_rfc3339(&e.timestamp) {
        Ok(ts) => ts.with_timezone(&Utc) >= cutoff,
        Err(_) => true,
    });
}

/// Load entries from a JSONL file in file order, skipping malformed lines.
/// Returns an empty list when the file is absent or unreadable. Pruning happens
/// on append, so this returns whatever is currently on disk.
fn load_entries(path: &Path) -> Vec<TranscriptEntry> {
    let mut entries: Vec<TranscriptEntry> = Vec::with_capacity(TRANSCRIPT_ALLOC_HINT);
    let Ok(data) = std::fs::read_to_string(path) else {
        return entries;
    };
    for (idx, raw_line) in data.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<TranscriptEntry>(line) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                warn!(
                    path = %path.display(),
                    line = idx.saturating_add(1),
                    error = %e,
                    "Skipping malformed transcript log line"
                );
            }
        }
    }
    entries
}

fn write_atomic(path: &Path, entries: &[TranscriptEntry]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut buf = String::with_capacity(entries.len().saturating_mul(256));
    for entry in entries {
        let line = serde_json::to_string(entry)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        buf.push_str(&line);
        buf.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shore_llm::types::{Timing, Usage};

    fn resp(text: &str, thinking: &str, model: &str, finish: &str) -> GenerateResponse {
        let mut blocks = Vec::new();
        if !thinking.is_empty() {
            blocks.push(ContentBlock::Thinking {
                thinking: thinking.to_owned(),
                signature: None,
            });
        }
        if !text.is_empty() {
            blocks.push(ContentBlock::Text {
                text: text.to_owned(),
            });
        }
        GenerateResponse {
            content: text.to_owned(),
            content_blocks: blocks,
            finish_reason: finish.to_owned(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 3,
                ..Default::default()
            },
            timing: Timing::default(),
            model: model.to_owned(),
        }
    }

    fn entry_at(iteration: u32, source: TranscriptSource, timestamp: String) -> TranscriptEntry {
        TranscriptEntry::from_response(
            source,
            "heartbeat",
            iteration,
            timestamp,
            Some("anthropic".to_owned()),
            &resp("hello", "let me think", "claude-x", "end_turn"),
            vec![TranscriptToolCall {
                name: "read".to_owned(),
                input: serde_json::json!({"path": "notes.md"}),
                output: "file contents".to_owned(),
                is_error: false,
            }],
        )
    }

    /// Entry stamped "now" so retention (a 14-day window from the real clock)
    /// never prunes it during a test run.
    fn entry(iteration: u32, source: TranscriptSource) -> TranscriptEntry {
        entry_at(iteration, source, Utc::now().to_rfc3339())
    }

    #[test]
    fn from_response_splits_reasoning_and_text() {
        let e = entry(0, TranscriptSource::Heartbeat);
        assert_eq!(e.reasoning, vec!["let me think".to_owned()]);
        assert_eq!(e.text, "hello");
        assert_eq!(e.model.as_deref(), Some("claude-x"));
        assert_eq!(e.provider.as_deref(), Some("anthropic"));
        assert_eq!(e.usage.cache_read_tokens, 3);
        assert_eq!(e.tool_calls.len(), 1);
    }

    #[test]
    fn append_then_read_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        append_entries(
            dir.path(),
            "alice",
            TranscriptSource::Heartbeat,
            &[entry(0, TranscriptSource::Heartbeat)],
        )
        .unwrap();
        append_entries(
            dir.path(),
            "alice",
            TranscriptSource::Heartbeat,
            &[entry(1, TranscriptSource::Heartbeat)],
        )
        .unwrap();

        let recent = read_recent(dir.path(), "alice", TranscriptSource::Heartbeat, 10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].iteration, 1, "newest first");
        assert_eq!(recent[1].iteration, 0);
    }

    #[test]
    fn sources_use_separate_files() {
        let dir = tempfile::tempdir().unwrap();
        append_entries(
            dir.path(),
            "alice",
            TranscriptSource::Heartbeat,
            &[entry(0, TranscriptSource::Heartbeat)],
        )
        .unwrap();
        assert!(read_recent(dir.path(), "alice", TranscriptSource::Dreaming, 10).is_empty());
        assert_eq!(
            read_recent(dir.path(), "alice", TranscriptSource::Heartbeat, 10).len(),
            1
        );
    }

    #[test]
    fn append_prunes_entries_older_than_window() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = entry_at(1, TranscriptSource::Heartbeat, Utc::now().to_rfc3339());
        let stale_ts = Utc::now()
            .checked_sub_signed(Duration::days(TRANSCRIPT_RETENTION_DAYS.saturating_add(6)))
            .unwrap()
            .to_rfc3339();
        let stale = entry_at(0, TranscriptSource::Heartbeat, stale_ts);
        append_entries(
            dir.path(),
            "alice",
            TranscriptSource::Heartbeat,
            &[stale, fresh],
        )
        .unwrap();

        let recent = read_recent(dir.path(), "alice", TranscriptSource::Heartbeat, 100);
        assert_eq!(recent.len(), 1, "stale entry should be pruned");
        assert_eq!(recent[0].iteration, 1);
    }

    #[test]
    fn retain_within_drops_old_keeps_recent_and_unparseable() {
        let cutoff = DateTime::parse_from_rfc3339("2026-06-14T00:00:00+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let mut entries = vec![
            entry_at(
                0,
                TranscriptSource::Heartbeat,
                "2026-06-01T00:00:00+00:00".to_owned(),
            ),
            entry_at(
                1,
                TranscriptSource::Heartbeat,
                "2026-06-20T00:00:00+00:00".to_owned(),
            ),
            entry_at(2, TranscriptSource::Heartbeat, "not-a-timestamp".to_owned()),
        ];
        retain_within(&mut entries, cutoff);
        let kept: Vec<u32> = entries.iter().map(|e| e.iteration).collect();
        // Old (Jun 1) dropped; recent (Jun 20) and unparseable kept.
        assert_eq!(kept, vec![1, 2]);
    }

    #[test]
    fn empty_append_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        append_entries(dir.path(), "alice", TranscriptSource::Heartbeat, &[]).unwrap();
        assert!(!transcript_log_path(dir.path(), "alice", TranscriptSource::Heartbeat).exists());
    }
}
