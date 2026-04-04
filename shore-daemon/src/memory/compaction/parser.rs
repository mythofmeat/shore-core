use super::types::{CompactedEntry, CompactionError};

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
pub const DEFAULT_COMPACT_PROMPT: &str = r#"You are recording what happened in this specific conversation between {{user}} and {{char}}. Write temporal, narrative entries — events, decisions, what was said, emotional shifts — anchored to this conversation. Do not extract timeless facts or stable preferences; those are handled separately.

Preserve:
- Key events and decisions made in this conversation
- Emotional developments and relationship changes
- Ongoing threads or unresolved topics
- Specific details that would be important to remember later
- If {{user}} corrected or updated previously stated information, note the change explicitly

Your response MUST contain two parts, in this order:

1. A single <recap> block — a flowing narrative (2-4 paragraphs) written **about {{char}} in close third person, using {{char}}'s own voice and vocabulary** — not "I" but "{{char}}" / "she" / "he" / "they". Same emotional texture, same interpretive lens, third-person pronouns. Cover what happened, how {{char}} felt about it, what matters to them, and where things stand with {{user}}.

{{#if recap}}
Here is the existing recap from previous compactions. Fold it into your new recap — preserve ongoing threads and relationship developments while incorporating new events. Older details should condense naturally but never disappear entirely:
<previous_recap>
{{recap}}
</previous_recap>
{{/if}}

<recap>
[rolling narrative recap, close third person about {{char}}]
</recap>

2. One or more <entry> blocks (one per topic discussed).

Each entry should be **atomic** — focused on exactly one topic or event. Prefer more entries with fewer bullets (2-4 each) over fewer entries with many bullets. If your bullets cover different subjects, split them into separate entries. Each entry is embedded as a single vector for retrieval, so mixing unrelated topics in one entry makes it harder to find later.

Both parts are required. Begin with the <recap>, then the <entry> blocks.

<entry>
<summary>
- [key fact or event, one per line]
- [preserve names, dates, specifics]
- [include emotional context where relevant]
</summary>
<topic_tags>
[comma separated short tags for this topic]
</topic_tags>
<entities>
- name: [entity name], type: [person/place/organization/concept], relation: [brief description of relation to the conversation]
</entities>
<memory_type>
[episodic or semantic — "episodic" for events, conversations, time-bound happenings; "semantic" for stable facts, preferences, traits, relationships]
</memory_type>
</entry>

If the entire conversation covers only one topic, produce a single <entry> block.

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

/// Extract all occurrences of `<tag>...</tag>` in the text.
pub(super) fn extract_all_xml_tags(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut results = Vec::new();
    let mut search_from = 0;
    while let Some(start) = text[search_from..].find(&open) {
        let abs_start = search_from + start + open.len();
        if let Some(end) = text[abs_start..].find(&close) {
            let content = text[abs_start..abs_start + end].trim();
            if !content.is_empty() {
                results.push(content.to_string());
            }
            search_from = abs_start + end + close.len();
        } else {
            break;
        }
    }
    results
}

/// Parse raw LLM response into recap + entries.
///
/// Expected format: `<recap>...</recap>` followed by one or more `<entry>...</entry>` blocks.
/// Each entry contains `<summary>`, `<topic_tags>`, and `<memory_type>` sub-tags.
pub fn parse_compaction_response(
    raw: &str,
) -> Result<(Option<String>, Vec<CompactedEntry>), CompactionError> {
    let recap = extract_xml_tag(raw, "recap");

    let entry_blocks = extract_all_xml_tags(raw, "entry");
    if entry_blocks.is_empty() {
        return Err(CompactionError::Parse(
            "no <entry> blocks found in LLM response".to_string(),
        ));
    }

    let mut entries = Vec::new();
    for block in &entry_blocks {
        let summary_text = extract_xml_tag(block, "summary").unwrap_or_default();
        let topic_tags = extract_xml_tag(block, "topic_tags").unwrap_or_default();
        let memory_type =
            extract_xml_tag(block, "memory_type").unwrap_or_else(|| "episodic".to_string());

        // Derive topic_key from the first tag.
        let topic_key = topic_tags
            .split(',')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase()
            .replace(' ', "_");

        entries.push(CompactedEntry {
            memory_type: memory_type.trim().to_string(),
            summary_text,
            topic_tags,
            topic_key,
            confidence: 0.9,
        });
    }

    Ok((recap, entries))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_xml_response() -> String {
        r#"<recap>
The assistant had a pleasant conversation with the user about their day and preferences.
They discussed daily activities and the user's beverage preferences.
</recap>

<entry>
<summary>
- User discussed their day
- They mentioned having a busy morning
</summary>
<topic_tags>daily, personal</topic_tags>
<memory_type>episodic</memory_type>
</entry>

<entry>
<summary>
- User prefers tea over coffee
- This is a stable preference
</summary>
<topic_tags>preference, food</topic_tags>
<memory_type>semantic</memory_type>
</entry>"#
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
    fn test_extract_all_xml_tags() {
        let text = "<entry>first</entry> middle <entry>second</entry>";
        let results = extract_all_xml_tags(text, "entry");
        assert_eq!(results, vec!["first", "second"]);
    }

    #[test]
    fn test_parse_compaction_response() {
        let raw = make_xml_response();
        let (recap, entries) = parse_compaction_response(&raw).unwrap();

        assert!(recap.is_some());
        assert!(recap.unwrap().contains("pleasant conversation"));
        assert_eq!(entries.len(), 2);
        assert!(entries[0].summary_text.contains("User discussed their day"));
        assert_eq!(entries[0].memory_type, "episodic");
        assert_eq!(entries[0].topic_tags, "daily, personal");
        assert!(entries[1].summary_text.contains("User prefers tea"));
        assert_eq!(entries[1].memory_type, "semantic");
    }

    #[test]
    fn test_parse_compaction_response_no_entries() {
        let raw = "<recap>Just a recap</recap>";
        let result = parse_compaction_response(raw);
        assert!(matches!(result, Err(CompactionError::Parse(_))));
    }

    #[test]
    fn test_parse_compaction_response_no_recap() {
        let raw = r#"<entry>
<summary>- Something happened</summary>
<topic_tags>test</topic_tags>
<memory_type>episodic</memory_type>
</entry>"#;
        let (recap, entries) = parse_compaction_response(raw).unwrap();
        assert!(recap.is_none());
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn test_parse_empty_topic_tags() {
        let raw = r#"<entry>
<summary>User discussed preferences</summary>
<topic_tags></topic_tags>
<memory_type>episodic</memory_type>
</entry>"#;
        let (_, entries) = parse_compaction_response(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].topic_tags, "");
        assert_eq!(
            entries[0].topic_key, "",
            "empty topic_tags produces empty topic_key"
        );
    }

    #[test]
    fn test_parse_unclosed_summary_tag() {
        let raw = r#"<entry>
<summary>Some text that never closes
<topic_tags>preferences</topic_tags>
<memory_type>episodic</memory_type>
</entry>"#;
        let (_, entries) = parse_compaction_response(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].summary_text, "",
            "unclosed summary tag falls to empty default"
        );
        assert_eq!(entries[0].topic_tags, "preferences");
    }

    #[test]
    fn test_parse_html_entities_passthrough() {
        let raw = r#"<entry>
<summary>User said &lt;hello&gt; &amp; goodbye</summary>
<topic_tags>greetings &amp; farewells</topic_tags>
<memory_type>episodic</memory_type>
</entry>"#;
        let (_, entries) = parse_compaction_response(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].summary_text.contains("&lt;hello&gt;"));
        assert!(entries[0].topic_tags.contains("&amp;"));
    }

    #[test]
    fn test_parse_prose_with_recap_no_entries() {
        let raw = r#"<recap>The conversation was about cats</recap>

The user discussed various topics including their preference for cats
over dogs. They also mentioned enjoying tea in the afternoon."#;
        let result = parse_compaction_response(raw);
        assert!(
            matches!(result, Err(CompactionError::Parse(ref msg)) if msg.contains("no <entry> blocks")),
            "prose-only response (with recap) should fail with Parse error"
        );
    }

    #[test]
    fn test_parse_entry_missing_memory_type() {
        let raw = r#"<entry>
<summary>User likes cats</summary>
<topic_tags>preferences,animals</topic_tags>
</entry>"#;
        let (_, entries) = parse_compaction_response(raw).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].memory_type, "episodic",
            "missing memory_type defaults to episodic"
        );
        assert_eq!(entries[0].summary_text, "User likes cats");
        assert_eq!(entries[0].topic_key, "preferences");
    }
}
