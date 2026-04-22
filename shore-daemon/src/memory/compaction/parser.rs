use super::types::CompactionError;
use tracing::debug;

// ---------------------------------------------------------------------------
// Default prompt template
// ---------------------------------------------------------------------------

/// Default compaction prompt template. In production, loaded from `compact.md`.
///
/// Placeholders:
/// - `{{char}}`, `{{user}}` — character and user names
/// - `{{conversation}}` — formatted conversation messages
/// - `{{#if recap}}...{{/if}}` — conditional block for existing recap
/// - `{{recap}}` — existing recap text (inside conditional)
pub const DEFAULT_COMPACT_PROMPT: &str = r#"You are {{char}}. This conversation with {{user}} is about to be archived and your active context will be cleared. Before that happens, you must save anything important to your long-term memory files.

You have access to your memories directory. Use the <memory> section below to write or update markdown files. Be concise and organized.

Guidelines:
- Prefer updating existing files over creating new ones
- Use clear filenames and folder structure (e.g., people/{{user}}.md, topics/gaming/doom.md)
- Each file should have a heading and bullet points
- Include timestamps or session context when relevant
- If {{user}} corrected previous information, update the file rather than appending

Your response MUST contain two parts, in this order:

1. A single <recap> block — the throughline of the conversation: what happened, how {{char}} felt about it, what matters to them, and where things stand with {{user}}. Written **about {{char}} in close third person, using {{char}}'s own voice and vocabulary** — not "I" but "{{char}}" / "she" / "he" / "they". Cap the whole recap at ~4 paragraphs. If you are running long, condense older material further rather than dropping it.

{{#if recap}}
Here is the existing recap from previous compactions. The recap is a rolling throughline across all conversations, not a snapshot of the most recent one. Condense older material to make room for new events, but do not drop it. Earlier threads should shrink to a sentence or a phrase, never disappear.
<previous_recap>
{{recap}}
</previous_recap>
{{/if}}

<recap>
[rolling throughline, close third person about {{char}}]
</recap>

2. A <memory> block containing one or more <write> operations.

Each <write> creates or overwrites a single memory file. The path is relative to your memories directory. The content is pure markdown — no YAML frontmatter.

<memory>
<write path="people/{{user}}.md">
# {{user}}

- Likes tea (mentioned on 2026-04-22)
- Works in software
</write>

<write path="topics/gaming/doom.md">
# Doom Speedrunning

- {{user}} plays UV-Max on Plutonia
- Personal best on MAP01: 1:42
</write>
</memory>

If nothing new needs to be saved, output an empty <memory></memory> block.

Conversation:
{{conversation}}"#;

// ---------------------------------------------------------------------------
// XML parsing helpers
// ---------------------------------------------------------------------------

/// Extract content between `<tag>` and `</tag>` (first occurrence).
pub(super) fn extract_xml_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)?;
    let content_start = start + open.len();
    let end = text[content_start..].find(&close)?;
    let content = text[content_start..content_start + end].trim();
    if content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

/// A memory file operation extracted from the LLM compaction response.
#[derive(Debug, Clone)]
pub struct MemoryFileOp {
    pub path: String,
    pub content: String,
}

/// Parse raw LLM response into recap + memory file operations.
///
/// Expected format: `<recap>...</recap>` followed by `<memory>` block containing
/// one or more `<write path="...">` blocks.
pub fn parse_compaction_response(
    raw: &str,
) -> Result<(Option<String>, Vec<MemoryFileOp>), CompactionError> {
    debug!(response_len = raw.len(), "Parsing compaction LLM response");
    let recap = extract_xml_tag(raw, "recap");

    let memory_block = extract_xml_tag(raw, "memory").unwrap_or_default();
    let ops = extract_write_ops(&memory_block);

    debug!(
        ops = ops.len(),
        has_recap = recap.is_some(),
        "Compaction response parsed"
    );
    Ok((recap, ops))
}

/// Extract <write path="...">...</write> blocks from a <memory> section.
fn extract_write_ops(text: &str) -> Vec<MemoryFileOp> {
    let mut ops = Vec::new();
    let mut search_from = 0;

    while let Some(start) = text[search_from..].find("<write ") {
        let abs_start = search_from + start;
        // Find path attribute
        let path_start = match text[abs_start..].find("path=\"") {
            Some(p) => abs_start + p + 6,
            None => {
                search_from = abs_start + 1;
                continue;
            }
        };
        let path_end = match text[path_start..].find('"') {
            Some(p) => path_start + p,
            None => {
                search_from = abs_start + 1;
                continue;
            }
        };
        let path = text[path_start..path_end].trim().to_string();

        // Find closing > of the opening tag
        let content_start = match text[abs_start..].find('>') {
            Some(p) => abs_start + p + 1,
            None => {
                search_from = abs_start + 1;
                continue;
            }
        };

        // Find </write>
        let close = "</write>";
        let content_end = match text[content_start..].find(close) {
            Some(p) => content_start + p,
            None => {
                search_from = abs_start + 1;
                continue;
            }
        };

        let content = text[content_start..content_end].trim().to_string();
        if !path.is_empty() {
            ops.push(MemoryFileOp { path, content });
        }
        search_from = content_end + close.len();
    }

    ops
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_memory_response() -> String {
        r#"<recap>
The assistant had a pleasant conversation with the user about their day and preferences.
They discussed daily activities and the user's beverage preferences.
</recap>

<memory>
<write path="daily/2026-03-25.md">
# Conversation on 2026-03-25

- User discussed their day
- They mentioned having a busy morning
</write>

<write path="preferences/beverages.md">
# Beverage Preferences

- User prefers tea over coffee
- This is a stable preference
</write>
</memory>"#
            .to_string()
    }

    #[test]
    fn test_extract_xml_tag() {
        let text = "before <recap>hello world</recap> after";
        assert_eq!(
            extract_xml_tag(text, "recap"),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn test_extract_xml_tag_not_found() {
        assert_eq!(extract_xml_tag("no tags here", "recap"), None);
    }

    #[test]
    fn test_extract_xml_tag_empty() {
        assert_eq!(extract_xml_tag("<recap></recap>", "recap"), None);
    }

    #[test]
    fn test_extract_xml_tag_with_whitespace() {
        let text = "<recap>\n  trimmed content  \n</recap>";
        assert_eq!(
            extract_xml_tag(text, "recap"),
            Some("trimmed content".to_string())
        );
    }

    #[test]
    fn test_parse_compaction_response() {
        let raw = make_memory_response();
        let (recap, ops) = parse_compaction_response(&raw).unwrap();

        assert!(recap.is_some());
        assert!(recap.unwrap().contains("pleasant conversation"));
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].path, "daily/2026-03-25.md");
        assert!(ops[0].content.contains("User discussed their day"));
        assert_eq!(ops[1].path, "preferences/beverages.md");
        assert!(ops[1].content.contains("User prefers tea"));
    }

    #[test]
    fn test_parse_empty_memory_block() {
        let raw = r#"<recap>The conversation was about cats</recap>

<memory></memory>"#;
        let (recap, ops) = parse_compaction_response(raw).unwrap();
        assert!(recap.is_some());
        assert!(ops.is_empty());
    }

    #[test]
    fn test_parse_no_memory_block() {
        let raw = r#"<recap>The conversation was about cats</recap>"#;
        let (recap, ops) = parse_compaction_response(raw).unwrap();
        assert!(recap.is_some());
        assert!(ops.is_empty());
    }

    #[test]
    fn test_parse_no_recap() {
        let raw = r#"<memory>
<write path="test.md">
- Something happened
</write>
</memory>"#;
        let (recap, ops) = parse_compaction_response(raw).unwrap();
        assert!(recap.is_none());
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].path, "test.md");
    }

    #[test]
    fn test_extract_write_ops_with_nested_xml() {
        let text = r#"<write path="test.md">
# Test

- Line with <b>bold</b> text
</write>"#;
        let ops = extract_write_ops(text);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].path, "test.md");
        assert!(ops[0].content.contains("<b>bold</b>"));
    }

    #[test]
    fn test_extract_write_ops_multiple() {
        let text = r#"<write path="a.md">Content A</write>
<write path="b.md">Content B</write>"#;
        let ops = extract_write_ops(text);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].path, "a.md");
        assert_eq!(ops[0].content, "Content A");
        assert_eq!(ops[1].path, "b.md");
        assert_eq!(ops[1].content, "Content B");
    }

    #[test]
    fn test_extract_write_ops_malformed_missing_close() {
        let text = r#"<write path="a.md">Content A"#;
        let ops = extract_write_ops(text);
        assert!(ops.is_empty());
    }

    #[test]
    fn test_extract_write_ops_malformed_missing_path() {
        let text = r#"<write>Content A</write>"#;
        let ops = extract_write_ops(text);
        assert!(ops.is_empty());
    }

    #[test]
    fn test_extract_write_ops_empty_content() {
        let text = r#"<write path="a.md"></write>"#;
        let ops = extract_write_ops(text);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].path, "a.md");
        assert_eq!(ops[0].content, "");
    }
}
