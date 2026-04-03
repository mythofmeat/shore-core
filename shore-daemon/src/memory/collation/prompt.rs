use crate::memory::db::Entry;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Default prompt template
// ---------------------------------------------------------------------------

pub const DEFAULT_REFINE_PROMPT: &str = r#"You are maintaining a memory database for an AI character chat system.

User: {{user}}
Character: {{char}}

{{definitions}}
## Your goal

Produce clean, well-tagged memory entries. Each entry should be atomic — focused on one specific event, person, preference, attribute, or relationship. Entries should be descriptive and tagged accurately.

## What you can do

For the CANDIDATE entries below, you may:
- **merge**: Combine 2+ candidate entries about the same topic into one consolidated entry. Prefer the most recent information as canonical, but preserve important historical context.
- **split**: Break one unfocused candidate entry into multiple atomic entries. Every piece of information from the original must appear in exactly one output.
- **update**: Rewrite a candidate entry's summary or tags for clarity, specificity, or accuracy without changing its scope.
- **keep** (default): If a candidate entry is already good, do nothing — omit it from your response.

## Rules

- You may ONLY act on entries marked [CANDIDATE]. Entries marked [CONTEXT] are reference only.
- When merging, prefer the more recent entry if entries conflict. If one explicitly corrects another, use the correction. If the conflict reflects genuine change over time, preserve both with temporal framing.
- Do not fabricate or infer anything not present in the source entries.
- Preserve entity names exactly as they appear. Do NOT rename, normalize, or merge entity names.
- Each output entry needs: summary_text, topic_tags (comma-separated), topic_key (single category), confidence (0.0-1.0).
- A merge must reference at least 2 source entry IDs.
- When splitting, every fact from the original must appear in exactly one output entry.

## Response format

Respond with ONLY a JSON object. Include ONLY entries that need changes.

{"actions":[
  {"action":"merge","source_entry_ids":["id1","id2"],"result":{"summary_text":"...","topic_tags":"...","topic_key":"...","confidence":0.9},"reason":"why"},
  {"action":"split","source_entry_id":"id1","results":[{"summary_text":"...","topic_tags":"...","topic_key":"...","confidence":0.85}],"reason":"why"},
  {"action":"update","entry_id":"id1","result":{"summary_text":"...","topic_tags":"...","topic_key":"...","confidence":0.9},"reason":"why"}
]}

If no changes are needed, return {"actions":[]}.

## Entries

### Candidates (you may modify these):
{{candidates}}

### Context (reference only, do NOT modify):
{{context}}"#;

// ---------------------------------------------------------------------------
// Prompt builder
// ---------------------------------------------------------------------------

/// Build a refine prompt from a template, replacing `{{candidates}}` and
/// `{{context}}` with formatted entry blocks, plus any template variables.
pub fn build_refine_prompt(
    template: &str,
    candidates: &[Entry],
    context: &[Entry],
    vars: &HashMap<String, String>,
) -> String {
    let format_entry = |e: &Entry, label: &str| -> String {
        let mut line = format!(
            "{} ID: {} | Tags: {} | Key: {} | Confidence: {:.2}",
            label, e.id, e.topic_tags, e.topic_key, e.confidence
        );
        if !e.start_timestamp.is_empty() {
            line.push_str(&format!(" | Time: {}", e.start_timestamp));
            if !e.end_timestamp.is_empty() && e.end_timestamp != e.start_timestamp {
                line.push_str(&format!("..{}", e.end_timestamp));
            }
        }
        line.push_str(&format!("\n  {}", e.summary_text));
        line
    };

    let mut cand_text = String::new();
    for e in candidates {
        cand_text.push_str(&format_entry(e, "[CANDIDATE]"));
        cand_text.push('\n');
    }

    let mut ctx_text = String::new();
    for e in context {
        ctx_text.push_str(&format_entry(e, "[CONTEXT]"));
        ctx_text.push('\n');
    }

    // Build definitions block from char_description / user_description vars.
    let mut defs = String::new();
    if let Some(cd) = vars.get("char_description").filter(|s| !s.is_empty()) {
        defs.push_str("## Character definition\n");
        defs.push_str(cd);
        defs.push_str("\n\n");
    }
    if let Some(ud) = vars.get("user_description").filter(|s| !s.is_empty()) {
        defs.push_str("## User definition\n");
        defs.push_str(ud);
        defs.push_str("\n\n");
    }

    let mut result = template
        .replace("{{definitions}}", &defs)
        .replace("{{candidates}}", &cand_text)
        .replace("{{context}}", &ctx_text);
    for (key, value) in vars {
        let tag = format!("{{{{{key}}}}}");
        result = result.replace(&tag, value);
    }
    result
}
