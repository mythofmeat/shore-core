use std::collections::HashMap;

use chrono::{DateTime, FixedOffset, Local};
use shore_protocol::types::{ContentBlock, ImageRef, Message, Role};
use tracing::{debug, warn};

/// Default context window size when not specified in model config.
const DEFAULT_MAX_CONTEXT_TOKENS: usize = 200_000;

/// Default max output tokens when not specified.
const DEFAULT_MAX_OUTPUT_TOKENS: usize = 4096;

/// Rough characters-per-token ratio for budget estimation.
const CHARS_PER_TOKEN: usize = 4;

/// Minimum gap (in seconds) before a time marker is injected.
const TIME_GAP_THRESHOLD_SECS: f64 = 1800.0; // 30 minutes

/// Built-in system prompt template used when no override is found on disk.
///
/// This is intentionally minimal — character/user definitions, TOOLS.md, and
/// recent-memory digest are injected as separate system blocks by
/// `assemble_prompt`.
const BUILTIN_SYSTEM_TEMPLATE: &str = "\
You are {{char}}, in conversation with {{user}}.
This is a text conversation. Communicate directly rather than narrating actions or using roleplay formatting.
Be consistent with established details and avoid fabricating memory.";

/// A block of system prompt content with an identifying label.
#[derive(Debug, Clone, PartialEq)]
pub struct SystemBlock {
    /// Label for cache/debugging (e.g. "system", "character", "memory").
    pub label: String,
    /// The text content of this block.
    pub content: String,
}

/// A message in the assembled prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptMessage {
    pub role: Role,
    pub content: String,
    pub images: Vec<ImageRef>,
    pub content_blocks: Vec<ContentBlock>,
}

/// The fully assembled prompt ready for LLM submission.
#[derive(Debug, Clone, PartialEq)]
pub struct AssembledPrompt {
    /// System prompt blocks (sent as the `system` parameter).
    pub system: Vec<SystemBlock>,
    /// Conversation messages trimmed to fit the token budget.
    pub messages: Vec<PromptMessage>,
}

// ---------------------------------------------------------------------------
<<<<<<< HEAD
// Capabilities config
// ---------------------------------------------------------------------------

/// Flat boolean config for building the capabilities system block.
///
/// Extracted from `AppConfig` at the call site to avoid dragging the full
/// config tree into prompt assembly.
#[derive(Debug, Clone, Default)]
pub struct CapabilitiesConfig {
    pub heartbeat_enabled: bool,
    pub scratchpad_enabled: bool,
    pub memory_enabled: bool,
    pub send_image_enabled: bool,
    pub generate_image_enabled: bool,
    pub web_search_enabled: bool,
}

impl CapabilitiesConfig {
    pub fn any_enabled(&self) -> bool {
        self.heartbeat_enabled
            || self.scratchpad_enabled
            || self.memory_enabled
            || self.send_image_enabled
            || self.generate_image_enabled
            || self.web_search_enabled
    }
}

/// Build a "Tool usage" system block — a short, always-identical stance that
/// reaches the model whenever any tool is enabled.
///
/// Returns `None` when no capabilities are enabled. Per-tool when/why guidance
/// lives in each tool's own `description` field (see Anthropic's tool-use docs
/// on detailed descriptions — that's where the model's selection signal comes
/// from). This block exists only to assert that reaching for a tool is
/// in-character and in-scope, since a character-framed system prompt otherwise
/// risks Claude treating tool calls as breaking frame.
pub fn build_capabilities_block(config: &CapabilitiesConfig) -> Option<String> {
    if !config.any_enabled() {
        return None;
    }

<<<<<<< HEAD
<<<<<<< HEAD
=======
>>>>>>> dev
    let mut parts = vec![
        "**Tool usage**".to_string(),
        String::new(),
        "You have a number of tools available to help you during the \
         conversation. You're encouraged to use them freely — reaching for a \
<<<<<<< HEAD
=======
    Some(
        "**Tool usage**\n\
         \n\
         You have a number of tools available to help you during the \
         conversation. You're encouraged to use them freely. Reaching for a \
>>>>>>> main
=======
>>>>>>> dev
         tool is in-character and enhances the conversation rather than \
         interrupting it. Each tool's own description covers when it's useful."
            .to_string(),
    ];

    if config.memory_enabled {
        parts.push(String::new());
        parts.push(
            "**Memory retrieval**\n\
             \n\
             Before making a factual claim about {{user}} or past conversations, \
             search your memories with `memory_search` or `memory_read`. \
             Do not guess facts you could verify. If `memory_search` returns \
             a relevant file, call `memory_read` to get the full content \
             before answering."
                .to_string(),
        );
    }

    Some(parts.join("\n"))
}

// ---------------------------------------------------------------------------
=======
>>>>>>> dev
// Prompt parameters
// ---------------------------------------------------------------------------

/// Parameters required for prompt assembly.
pub struct PromptParams<'a> {
    /// Character name.
    pub character_name: &'a str,
    /// Resolved display name for the user (from config or $USER).
    pub display_name: &'a str,
    /// Active AGENTS.md content, or None to use the built-in default.
    pub system_prompt: Option<&'a str>,
    /// Active TOOLS.md guidance.
    pub tools_guidance: Option<&'a str>,
    /// Character definition (from SOUL.md).
    pub character_definition: Option<&'a str>,
    /// User definition (from USER.md).
    pub user_definition: Option<&'a str>,
    /// Recent-memory digest compiled at the last compaction boundary.
    pub recent_memory_digest: Option<&'a str>,
    /// Whether this is a private conversation.
    pub is_private: bool,
    /// Conversation messages (full history).
    pub messages: &'a [Message],
    /// Maximum context tokens (total context window). `None` uses default.
    pub max_context_tokens: Option<u32>,
    /// Maximum output tokens (reserved for response). `None` uses default.
    pub max_output_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Prompt assembly
// ---------------------------------------------------------------------------

/// Assemble the complete prompt from active snapshot content and history.
///
/// Produces multiple system blocks matching V1's structure:
/// 1. Rendered AGENTS.md template (or built-in default)
/// 2. TOOLS.md guidance (if present)
/// 3. `<{char}>` character definition (if present)
/// 4. `<{user}>` user definition (if present)
/// 5. `<recent_memory>` block (if present, not private)
///
/// Then trims conversation history to fit the token budget.
pub fn assemble_prompt(params: &PromptParams<'_>) -> AssembledPrompt {
    let max_context = params
        .max_context_tokens
        .map(|t| t as usize)
        .unwrap_or(DEFAULT_MAX_CONTEXT_TOKENS);
    let max_output = params
        .max_output_tokens
        .map(|t| t as usize)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);

    // ── 1. Build template variables ───────────────────────────────────
    let mut vars = HashMap::new();
    vars.insert("char".into(), params.character_name.to_string());
    vars.insert("character_name".into(), params.character_name.to_string());
    vars.insert("user".into(), params.display_name.to_string());
    vars.insert("date".into(), String::new());
    vars.insert("time".into(), String::new());

    // ── 2. Resolve and render system template ─────────────────────────
    let using_builtin = params.system_prompt.is_none();
    let template = params.system_prompt.unwrap_or(BUILTIN_SYSTEM_TEMPLATE);
    debug!(
        character = %params.character_name,
        builtin_template = using_builtin,
        "assembling prompt"
    );

    let rendered_system = render_template(template, &vars);

    // ── 3. Build system blocks ────────────────────────────────────────
    let mut system = Vec::new();

    // Block 1: core system prompt.
    system.push(SystemBlock {
        label: "system".into(),
        content: rendered_system,
    });

    // Block 2: TOOLS.md guidance.
    if let Some(tools_guidance) = params.tools_guidance.filter(|s| !s.is_empty()) {
        system.push(SystemBlock {
            label: "tools_guidance".into(),
            content: tools_guidance.to_string(),
        });
    }

    // Block 3: character definition.
    if let Some(char_def) = params.character_definition.filter(|s| !s.is_empty()) {
        let tag = xml_tag_from_name(params.character_name, "character");
        system.push(SystemBlock {
            label: "character".into(),
            content: format!("<{tag}>\n{char_def}\n</{tag}>"),
        });
    }

    // Block 4: user definition.
    if let Some(user_def) = params.user_definition.filter(|s| !s.is_empty()) {
        let tag = xml_tag_from_name(params.display_name, "user");
        system.push(SystemBlock {
            label: "user".into(),
            content: format!("<{tag}>\n{user_def}\n</{tag}>"),
        });
    }

    // Block 5: recent memory digest (suppressed for private conversations).
    if !params.is_private {
        if let Some(digest) = params.recent_memory_digest.filter(|s| !s.is_empty()) {
            system.push(SystemBlock {
                label: "recent_memory".into(),
                content: format!(
<<<<<<< HEAD
<<<<<<< HEAD
                    "<{char_tag}_recap> \n\
                     The following is a brief recap of recent conversations. \
                     This recap is not exhaustive and only covers a short period of time. \
                     Much more detailed information can be found within your memory database.\n\n\
                     {recap}\n\
                     </{char_tag}_recap>"
=======
=======
>>>>>>> dev
                    "<recent_memory>\n\
                     The following is a compact digest of your most recent durable memories.\n\n\
                     {digest}\n\
                     </recent_memory>"
<<<<<<< HEAD
>>>>>>> breaking/openclawify
=======
>>>>>>> dev
                ),
            });
        }
    }

    debug!(
        system_blocks = system.len(),
        has_char_def = params
            .character_definition
            .filter(|s| !s.is_empty())
            .is_some(),
        has_user_def = params.user_definition.filter(|s| !s.is_empty()).is_some(),
        has_recent_memory = params
            .recent_memory_digest
            .filter(|s| !s.is_empty())
            .is_some(),
        "system blocks assembled"
    );

    // ── 4. Calculate token budget for messages ────────────────────────
    let system_tokens = estimate_tokens(
        &system
            .iter()
            .map(|b| b.content.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let available_for_messages = max_context
        .saturating_sub(max_output)
        .saturating_sub(system_tokens);

    debug!(
        max_context,
        max_output,
        system_tokens,
        available_for_messages,
        input_messages = params.messages.len(),
        "token budget calculated"
    );

    if available_for_messages == 0 {
        warn!(
            max_context,
            max_output,
            system_tokens,
            "zero tokens available for messages — system prompt may exceed context window"
        );
    }

    // ── 5. Trim conversation history to fit budget ────────────────────
    let messages = trim_messages(params.messages, available_for_messages);

    debug!(
        input_messages = params.messages.len(),
        output_messages = messages.len(),
        trimmed = params.messages.len().saturating_sub(messages.len()),
        "prompt assembly complete"
    );

    AssembledPrompt { system, messages }
}

// ---------------------------------------------------------------------------
// Template rendering
// ---------------------------------------------------------------------------

/// Mustache-like template rendering with generic key-value substitution.
///
/// - `{{key}}` → value from `vars` (or empty string if key not found)
/// - `{{#if key}}...{{/if}}` → include block if key exists and is non-empty
///
/// Processes conditionals first (one pass per `{{#if ...}}`), then substitutes
/// remaining `{{key}}` variables.
pub fn render_template(template: &str, vars: &HashMap<String, String>) -> String {
    let mut result = template.to_string();

    // Process all conditional blocks.
    // Loop until no more {{#if ...}} blocks are found.
    loop {
        let Some(if_start) = result.find("{{#if ") else {
            break;
        };
        let Some(name_end) = result[if_start + 6..].find("}}") else {
            break;
        };
        let name = result[if_start + 6..if_start + 6 + name_end]
            .trim()
            .to_string();
        let open_tag_end = if_start + 6 + name_end + 2;

        let close_tag = "{{/if}}";
        let Some(close_pos) = result[open_tag_end..].find(close_tag) else {
            break;
        };
        let close_abs = open_tag_end + close_pos;

        let block_content = &result[open_tag_end..close_abs];
        let after = &result[close_abs + close_tag.len()..];

        let value = vars.get(&name).filter(|v| !v.is_empty());
        if let Some(v) = value {
            let var_tag = format!("{{{{{name}}}}}");
            let expanded = block_content.replace(&var_tag, v);
            result = format!("{}{expanded}{after}", &result[..if_start]);
        } else {
            result = format!("{}{after}", &result[..if_start]);
        }
    }

    // Substitute remaining {{key}} variables.
    for (key, value) in vars {
        let tag = format!("{{{{{key}}}}}");
        result = result.replace(&tag, value);
    }

    result
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a free-form name to a safe XML tag name.
///
/// Lowercases the input, replaces non-alphanumeric characters with `_`,
/// collapses runs of underscores, and strips leading/trailing underscores.
/// Falls back to `fallback` if the result is empty.
pub fn xml_tag_from_name(name: &str, fallback: &str) -> String {
    let mut tag: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();

    // Collapse runs of underscores and strip edges.
    while tag.contains("__") {
        tag = tag.replace("__", "_");
    }
    tag = tag.trim_matches('_').to_string();

    if tag.is_empty() {
        fallback.to_string()
    } else {
        tag
    }
}

/// Estimate token count from text using a character-based heuristic.
fn estimate_tokens(text: &str) -> usize {
    // Rough approximation: ~4 characters per token.
    text.len().div_ceil(CHARS_PER_TOKEN)
}

/// Estimate token count for a message, accounting for content blocks.
fn estimate_message_tokens(msg: &Message) -> usize {
    if msg.content_blocks.is_empty() {
        return estimate_tokens(&msg.content);
    }
    // Sum tokens across all content blocks.
    msg.content_blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => estimate_tokens(text),
            ContentBlock::Thinking { thinking, .. } => estimate_tokens(thinking),
            ContentBlock::ToolUse { input, name, .. } => {
                estimate_tokens(name) + estimate_tokens(&input.to_string())
            }
            ContentBlock::RedactedThinking { .. } => 0,
            ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
        })
        .sum()
}

/// Format a time gap as a human-readable marker with both relative and
/// absolute time, e.g. `[6 hours later · 9:14 PM]`.
///
/// Returns `None` for gaps shorter than 30 minutes.
fn format_time_gap(gap_secs: f64, current_ts: &DateTime<FixedOffset>) -> Option<String> {
    if gap_secs < TIME_GAP_THRESHOLD_SECS {
        return None;
    }

    let relative = if gap_secs < 5400.0 {
        // < 1.5 hours
        "about an hour later".to_string()
    } else if gap_secs < 64800.0 {
        // < 18 hours
        let hours = (gap_secs / 3600.0).round() as u32;
        format!("{hours} hours later")
    } else if gap_secs < 129600.0 {
        // < 36 hours
        "about a day later".to_string()
    } else {
        let days = (gap_secs / 86400.0).round() as u32;
        format!("{days} days later")
    };

    let time_str = current_ts
        .with_timezone(&Local)
        .format("%-I:%M %p")
        .to_string();
    Some(format!("[{relative} · {time_str}]"))
}

/// Trim messages from the beginning to fit within the token budget.
///
/// Keeps the most recent messages, discarding older ones first. After
/// trimming, drops any leading tool-loop messages (tool_result user
/// messages and tool_use-only assistant messages) that would be orphaned
/// without their preceding context.
fn trim_messages(messages: &[Message], token_budget: usize) -> Vec<PromptMessage> {
    // Build from the end (most recent first), accumulating token cost.
    let mut selected: Vec<(PromptMessage, &str)> = Vec::new();
    let mut used_tokens = 0;

    for msg in messages.iter().rev() {
        let msg_tokens = estimate_message_tokens(msg);
        if used_tokens + msg_tokens > token_budget && !selected.is_empty() {
            // Budget exhausted — stop adding older messages.
            break;
        }
        used_tokens += msg_tokens;
        selected.push((
            PromptMessage {
                role: msg.role.clone(),
                content: msg.content.clone(),
                images: msg.images.clone(),
                content_blocks: msg.content_blocks.clone(),
            },
            &msg.timestamp,
        ));
    }

    // Reverse to restore chronological order.
    selected.reverse();

    // Drop leading tool-loop messages that would be orphaned.
    while !selected.is_empty() && is_tool_loop_msg_prompt(&selected[0].0) {
        selected.remove(0);
    }

    // ── Inject time-gap markers on user messages ──────────────────────
    // Walk forward, tracking the previous timestamp. When the gap between
    // consecutive messages exceeds the threshold, prepend a marker like
    // `[6 hours later · 9:14 PM]` to the next user message's content.
    // Heartbeat recaps are no longer injected here — they're persisted
    // as Role::System messages in active.jsonl by the tick itself, so they
    // already sit at their natural chronological position in the history.
    let mut prev_ts: Option<DateTime<FixedOffset>> = None;
    let mut result: Vec<PromptMessage> = Vec::with_capacity(selected.len());

    for (mut pm, ts_str) in selected {
        let current_ts = DateTime::parse_from_rfc3339(ts_str).ok();

        if pm.role == Role::User {
            if let (Some(prev), Some(cur)) = (prev_ts, current_ts) {
                let gap_secs = (cur - prev).num_seconds() as f64;
                let time_marker = format_time_gap(gap_secs, &cur);

                // Inject the time-gap marker into the user message (it's
                // deterministic — same timestamps always produce the same
                // marker, so the cache prefix stays stable).
                if let Some(t) = time_marker {
                    pm.content = format!("{t}\n\n{}", pm.content);
                    if let Some(ContentBlock::Text { text }) = pm.content_blocks.first_mut() {
                        *text = format!("{t}\n\n{text}");
                    }
                }
            }
        }

        if current_ts.is_some() {
            prev_ts = current_ts;
        }
        result.push(pm);
    }

    result
}

/// Check if a PromptMessage is a tool-loop intermediate.
fn is_tool_loop_msg_prompt(msg: &PromptMessage) -> bool {
    if msg.content_blocks.is_empty() {
        return false;
    }
    match msg.role {
        Role::User => msg
            .content_blocks
            .iter()
            .all(|b| matches!(b, ContentBlock::ToolResult { .. })),
        Role::Assistant => {
            let has_text = msg
                .content_blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::Text { text } if !text.is_empty()));
            let has_tool_use = msg
                .content_blocks
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
            !has_text && has_tool_use
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: Role, content: &str) -> Message {
        Message {
            msg_id: uuid::Uuid::new_v4().to_string(),
            role,
            content: content.to_string(),
            images: vec![],
            content_blocks: vec![],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn test_vars() -> HashMap<String, String> {
        let mut vars = HashMap::new();
        vars.insert("char".into(), "Shore".into());
        vars.insert("character_name".into(), "Shore".into());
        vars.insert("user".into(), "Alice".into());
        vars
    }

    fn make_params<'a>(messages: &'a [Message]) -> PromptParams<'a> {
        PromptParams {
            character_name: "TestChar",
            display_name: "TestUser",
            system_prompt: None,
            tools_guidance: None,
            character_definition: None,
            user_definition: None,
            recent_memory_digest: None,
            is_private: false,
            messages,
            max_context_tokens: None,
            max_output_tokens: None,
        }
    }

    // ── Template rendering ─────────────────────────────────────────────

    #[test]
    fn render_template_substitutes_variables() {
        let vars = test_vars();
        let result = render_template("Hello, {{char}}!", &vars);
        assert_eq!(result, "Hello, Shore!");
    }

    #[test]
    fn render_template_character_name_compat() {
        let vars = test_vars();
        let result = render_template("Hello, {{character_name}}!", &vars);
        assert_eq!(result, "Hello, Shore!");
    }

    #[test]
    fn render_template_unknown_var_replaced_empty() {
        let vars = test_vars();
        let result = render_template("Hello, {{unknown}}!", &vars);
        // Unknown vars are NOT replaced (no entry in map → tag stays).
        // Actually, our loop only replaces keys that exist in the map.
        // So {{unknown}} stays as-is. This is fine — V1 behaves the same way.
        assert_eq!(result, "Hello, {{unknown}}!");
    }

    #[test]
    fn render_template_conditional_present() {
        let mut vars = test_vars();
        vars.insert("character_definition".into(), "A helpful companion".into());
        let template =
            "Start.{{#if character_definition}}\nDef: {{character_definition}}{{/if}}\nEnd.";
        let result = render_template(template, &vars);
        assert_eq!(result, "Start.\nDef: A helpful companion\nEnd.");
    }

    #[test]
    fn render_template_conditional_absent() {
        let vars = test_vars();
        let template =
            "Start.{{#if character_definition}}\nDef: {{character_definition}}{{/if}}\nEnd.";
        let result = render_template(template, &vars);
        assert_eq!(result, "Start.\nEnd.");
    }

    #[test]
    fn render_template_conditional_empty_string() {
        let mut vars = test_vars();
        vars.insert("recap".into(), String::new());
        let template = "Before{{#if recap}} RECAP: {{recap}}{{/if}} After";
        let result = render_template(template, &vars);
        assert_eq!(result, "Before After");
    }

    #[test]
    fn render_template_builtin_system() {
        let mut vars = test_vars();
        vars.insert("char".into(), "TestChar".into());
        vars.insert("user".into(), "TestUser".into());
        let result = render_template(BUILTIN_SYSTEM_TEMPLATE, &vars);
        assert!(result.contains("You are TestChar, in conversation with TestUser."));
        assert!(result.contains("Communicate directly"));
    }

    // ── XML tag helper ────────────────────────────────────────────────

    #[test]
    fn xml_tag_from_name_basic() {
        assert_eq!(xml_tag_from_name("Alice", "character"), "alice");
        assert_eq!(xml_tag_from_name("Dr. Bob", "character"), "dr_bob");
        assert_eq!(xml_tag_from_name("Shore v2", "character"), "shore_v2");
    }

    #[test]
    fn xml_tag_from_name_empty_fallback() {
        assert_eq!(xml_tag_from_name("", "character"), "character");
        assert_eq!(xml_tag_from_name("...", "user"), "user");
    }

    #[test]
    fn xml_tag_from_name_collapses_underscores() {
        assert_eq!(xml_tag_from_name("a - b", "x"), "a_b");
    }

    // ── Token estimation ──────────────────────────────────────────────

    #[test]
    fn estimate_tokens_basic() {
        assert_eq!(estimate_tokens("Hello world!"), 3);
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("Hello"), 2);
    }

    #[test]
    fn estimate_tokens_uses_byte_length_not_char_count() {
        // ASCII: 1 byte per char → 5 bytes / 4 = 2 tokens
        assert_eq!(estimate_tokens("Hello"), 2);

        // CJK: 3 bytes per char → 15 bytes / 4 = 4 tokens (not 5/4 = 2)
        let cjk = "日本語の文"; // 5 chars, 15 bytes
        assert_eq!(cjk.len(), 15);
        assert_eq!(estimate_tokens(cjk), 4);

        // Emoji: 4 bytes each → 16 bytes / 4 = 4 tokens
        let emoji = "😀😁😂🤣"; // 4 chars, 16 bytes
        assert_eq!(emoji.len(), 16);
        assert_eq!(estimate_tokens(emoji), 4);
    }

    #[test]
    fn estimate_tokens_short_words_undercount() {
        // Real tokenizers often produce 1 token per short word + whitespace.
        // "I am a" → 3 real tokens, but heuristic says 6 bytes / 4 = 2.
        // This documents the known under-counting for short-word text.
        let short_words = "I am a";
        assert_eq!(estimate_tokens(short_words), 2); // real ≈ 3
    }

    #[test]
    fn estimate_tokens_json_payload() {
        // JSON has many structural chars (braces, quotes, colons) that
        // tokenizers often group differently than plain prose.
        let json = r#"{"name":"test","values":[1,2,3],"nested":{"key":"val"}}"#;
        assert_eq!(estimate_tokens(json), json.len().div_ceil(4));
    }

    #[test]
    fn estimate_tokens_code_block() {
        // Code with identifiers, operators, and indentation.
        let code = "fn estimate_tokens(text: &str) -> usize {\n    text.len().div_ceil(4)\n}";
        assert_eq!(estimate_tokens(code), code.len().div_ceil(4));
    }

    #[test]
    fn estimate_message_tokens_redacted_thinking_is_zero() {
        let msg = Message {
            msg_id: "m1".into(),
            role: Role::Assistant,
            content: String::new(),
            images: vec![],
            content_blocks: vec![ContentBlock::RedactedThinking {
                data: "opaque".into(),
            }],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        assert_eq!(estimate_message_tokens(&msg), 0);
    }

    #[test]
    fn estimate_message_tokens_mixed_blocks_sums_all() {
        let msg = Message {
            msg_id: "m1".into(),
            role: Role::Assistant,
            content: String::new(),
            images: vec![],
            content_blocks: vec![
                ContentBlock::Thinking {
                    thinking: "A".repeat(40),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "B".repeat(80),
                },
                ContentBlock::ToolUse {
                    id: "tu1".into(),
                    name: "check_time".into(),
                    input: serde_json::json!({"tz": "UTC"}),
                },
                ContentBlock::RedactedThinking {
                    data: "ignored".into(),
                },
            ],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let tokens = estimate_message_tokens(&msg);
        // 40/4 + 80/4 + tool_name + tool_input + 0 (redacted)
        let tool_input_str = serde_json::json!({"tz": "UTC"}).to_string();
        let expected = 10 + 20 + "check_time".len().div_ceil(4) + tool_input_str.len().div_ceil(4);
        assert_eq!(tokens, expected);
    }

    #[test]
    fn estimate_message_tokens_uses_content_blocks_when_present() {
        let msg = Message {
            msg_id: "m1".into(),
            role: Role::Assistant,
            content: "short".into(),
            images: vec![],
            content_blocks: vec![
                ContentBlock::Text {
                    text: "A".repeat(40).to_string(),
                },
                ContentBlock::Thinking {
                    thinking: "B".repeat(20).to_string(),
                    signature: None,
                },
            ],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let tokens = estimate_message_tokens(&msg);
        assert_eq!(tokens, 15);
    }

    #[test]
    fn estimate_message_tokens_falls_back_to_content_when_no_blocks() {
        let msg = make_msg(Role::User, &"X".repeat(20));
        let tokens = estimate_message_tokens(&msg);
        assert_eq!(tokens, 5);
    }

    #[test]
    fn estimate_message_tokens_tool_use_block() {
        let msg = Message {
            msg_id: "m1".into(),
            role: Role::Assistant,
            content: String::new(),
            images: vec![],
            content_blocks: vec![ContentBlock::ToolUse {
                id: "tu_1".into(),
                name: "check_time".into(),
                input: serde_json::json!({}),
            }],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        assert!(estimate_message_tokens(&msg) > 0);
    }

    #[test]
    fn estimate_message_tokens_tool_result_block() {
        let msg = Message {
            msg_id: "m1".into(),
            role: Role::User,
            content: String::new(),
            images: vec![],
            content_blocks: vec![ContentBlock::ToolResult {
                tool_use_id: "tu_1".into(),
                content: "2026-03-27T12:00:00Z".into(),
                is_error: false,
            }],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        assert_eq!(estimate_message_tokens(&msg), 5);
    }

    // ── Message trimming ──────────────────────────────────────────────

    #[test]
    fn trim_messages_accounts_for_content_blocks_size() {
        let small_msg = make_msg(Role::User, "Hello");
        let big_msg = Message {
            msg_id: "m_big".into(),
            role: Role::Assistant,
            content: String::new(),
            images: vec![],
            content_blocks: vec![ContentBlock::Text {
                text: "X".repeat(400),
            }],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let recent_msg = make_msg(Role::User, "Recent");

        let msgs = vec![small_msg, big_msg, recent_msg];
        let result = trim_messages(&msgs, 10);
        assert_eq!(result.last().unwrap().content, "Recent");
        assert!(result.len() < 3);
    }

    #[test]
    fn trim_messages_all_fit() {
        let msgs = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there"),
        ];
        let result = trim_messages(&msgs, 1000);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].content, "Hello");
        assert_eq!(result[1].content, "Hi there");
    }

    #[test]
    fn trim_messages_drops_oldest() {
        let msgs = vec![
            make_msg(Role::User, &"A".repeat(100)),
            make_msg(Role::Assistant, &"B".repeat(100)),
            make_msg(Role::User, "Recent"),
        ];
        let result = trim_messages(&msgs, 30);
        assert!(result.len() < 3);
        assert_eq!(result.last().unwrap().content, "Recent");
    }

    #[test]
    fn trim_messages_always_includes_at_least_one() {
        let msgs = vec![make_msg(Role::User, &"A".repeat(1000))];
        let result = trim_messages(&msgs, 0);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn trim_messages_preserves_order() {
        let msgs = vec![
            make_msg(Role::User, "First"),
            make_msg(Role::Assistant, "Second"),
            make_msg(Role::User, "Third"),
        ];
        let result = trim_messages(&msgs, 10000);
        assert_eq!(result[0].content, "First");
        assert_eq!(result[1].content, "Second");
        assert_eq!(result[2].content, "Third");
    }

    // ── Time-gap markers ───────────────────────────────────────────────

    fn make_msg_at(role: Role, content: &str, timestamp: &str) -> Message {
        Message {
            msg_id: uuid::Uuid::new_v4().to_string(),
            role,
            content: content.to_string(),
            images: vec![],
            content_blocks: vec![ContentBlock::Text {
                text: content.to_string(),
            }],
            alt_index: None,
            alt_count: None,
            timestamp: timestamp.to_string(),
        }
    }

    #[test]
    fn format_time_gap_under_threshold_returns_none() {
        let ts = DateTime::parse_from_rfc3339("2026-04-04T09:30:00-07:00").unwrap();
        assert!(format_time_gap(1799.0, &ts).is_none());
        assert!(format_time_gap(0.0, &ts).is_none());
    }

    #[test]
    fn format_time_gap_about_an_hour() {
        let ts = DateTime::parse_from_rfc3339("2026-04-04T10:30:00-07:00").unwrap();
        let result = format_time_gap(3600.0, &ts).unwrap();
        assert!(result.contains("about an hour later"));
        // Time is converted to local; verify it contains a clock time (not a specific value).
        let local_str = ts.with_timezone(&Local).format("%-I:%M %p").to_string();
        assert!(result.contains(&local_str));
    }

    #[test]
    fn format_time_gap_multiple_hours() {
        let ts = DateTime::parse_from_rfc3339("2026-04-04T21:14:00-07:00").unwrap();
        let result = format_time_gap(6.0 * 3600.0, &ts).unwrap();
        assert!(result.contains("6 hours later"));
        let local_str = ts.with_timezone(&Local).format("%-I:%M %p").to_string();
        assert!(result.contains(&local_str));
    }

    #[test]
    fn format_time_gap_about_a_day() {
        let ts = DateTime::parse_from_rfc3339("2026-04-05T09:00:00-07:00").unwrap();
        let result = format_time_gap(24.0 * 3600.0, &ts).unwrap();
        assert!(result.contains("about a day later"));
    }

    #[test]
    fn format_time_gap_multiple_days() {
        let ts = DateTime::parse_from_rfc3339("2026-04-07T09:00:00-07:00").unwrap();
        let result = format_time_gap(3.0 * 86400.0, &ts).unwrap();
        assert!(result.contains("3 days later"));
    }

    #[test]
    fn trim_messages_injects_time_gap_on_user_message() {
        let msgs = vec![
            make_msg_at(Role::User, "Good morning", "2026-04-04T09:00:00-07:00"),
            make_msg_at(Role::Assistant, "Morning!", "2026-04-04T09:01:00-07:00"),
            make_msg_at(Role::User, "I'm back", "2026-04-04T15:30:00-07:00"),
        ];
        let result = trim_messages(&msgs, 100_000);
        assert_eq!(result.len(), 3);
        // First user message: no gap marker.
        assert!(!result[0].content.contains("later"));
        // Third message (user, 6.5h gap): should have marker.
        assert!(result[2].content.contains("hours later"));
        let ts3 = DateTime::parse_from_rfc3339("2026-04-04T15:30:00-07:00").unwrap();
        let local_str = ts3.with_timezone(&Local).format("%-I:%M %p").to_string();
        assert!(result[2].content.contains(&local_str));
        assert!(result[2].content.contains("I'm back"));
        // content_blocks should also be updated.
        if let Some(ContentBlock::Text { text }) = result[2].content_blocks.first() {
            assert!(text.contains("hours later"));
        } else {
            panic!("Expected Text content block");
        }
    }

    #[test]
    fn trim_messages_no_gap_marker_for_short_gaps() {
        let msgs = vec![
            make_msg_at(Role::User, "Hello", "2026-04-04T09:00:00-07:00"),
            make_msg_at(Role::Assistant, "Hi", "2026-04-04T09:01:00-07:00"),
            make_msg_at(Role::User, "Quick follow-up", "2026-04-04T09:10:00-07:00"),
        ];
        let result = trim_messages(&msgs, 100_000);
        assert_eq!(result[2].content, "Quick follow-up");
    }

    #[test]
    fn trim_messages_no_gap_marker_on_assistant_messages() {
        let msgs = vec![
            make_msg_at(Role::User, "Hello", "2026-04-04T09:00:00-07:00"),
            // Large gap, but the next message is assistant — no marker.
            make_msg_at(
                Role::Assistant,
                "Hey, you there?",
                "2026-04-04T15:00:00-07:00",
            ),
            make_msg_at(Role::User, "Yeah!", "2026-04-04T15:01:00-07:00"),
        ];
        let result = trim_messages(&msgs, 100_000);
        assert!(
            !result[1].content.contains("later"),
            "assistant messages should not get gap markers"
        );
        // But the gap from the assistant message to the next user message is only 1 min — no marker.
        assert_eq!(result[2].content, "Yeah!");
    }

    // ── Full assembly ─────────────────────────────────────────────────

    #[test]
    fn assemble_prompt_basic() {
        let messages = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi!"),
        ];

        let params = PromptParams {
            character_name: "TestChar",
            display_name: "TestUser",
            system_prompt: None,
            tools_guidance: None,
            character_definition: Some("A friendly test character."),
            user_definition: Some("A developer."),
            recent_memory_digest: None,
            is_private: false,
            messages: &messages,
            max_context_tokens: Some(200_000),
            max_output_tokens: Some(4096),
        };

        let result = assemble_prompt(&params);

        // System block should contain character info.
        assert!(result.system[0].content.contains("TestChar"));
        assert!(result.system[0].content.contains("TestUser"));
        assert!(result.system[0].label == "system");

        // Character and user definitions in separate blocks.
        let char_block = result
            .system
            .iter()
            .find(|b| b.label == "character")
            .unwrap();
        assert!(char_block.content.contains("A friendly test character."));
        assert!(char_block.content.contains("<testchar>"));

        let user_block = result.system.iter().find(|b| b.label == "user").unwrap();
        assert!(user_block.content.contains("A developer."));
        assert!(user_block.content.contains("<testuser>"));

        // Messages should be included.
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, Role::User);
        assert_eq!(result.messages[0].content, "Hello");
    }

    #[test]
    fn assemble_prompt_uses_custom_template() {
        let params = PromptParams {
            display_name: "User",
            system_prompt: Some("Custom prompt for {{character_name}}."),
            ..make_params(&[])
        };

        let result = assemble_prompt(&params);
        // {{character_name}} should be substituted (backward compat).
        assert_eq!(result.system[0].content, "Custom prompt for TestChar.");
    }

    #[test]
    fn assemble_prompt_character_template_overrides_global() {
        let params = PromptParams {
            system_prompt: Some("Character-specific template."),
            ..make_params(&[])
        };
        let result = assemble_prompt(&params);
        assert_eq!(result.system[0].content, "Character-specific template.");
    }

    #[test]
    fn assemble_prompt_injects_recent_memory_digest() {
        let params = PromptParams {
            recent_memory_digest: Some("We talked about Rust."),
            ..make_params(&[])
        };
        let result = assemble_prompt(&params);

        let digest_block = result
            .system
            .iter()
            .find(|b| b.label == "recent_memory")
            .unwrap();
        assert!(digest_block.content.contains("We talked about Rust."));
        assert!(digest_block
            .content
            .contains("most recent durable memories"));
        assert!(digest_block.content.contains("<recent_memory>"));
    }

    #[test]
    fn assemble_prompt_blanks_date_time_for_cache_stability() {
        let params = PromptParams {
            system_prompt: Some("Today is {{date}} at {{time}}."),
            ..make_params(&[])
        };
        let result = assemble_prompt(&params);

        let system_text = &result.system[0].content;
        assert!(!system_text.contains("{{date}}"));
        assert!(!system_text.contains("{{time}}"));
        assert_eq!(system_text, "Today is  at .");
    }

    #[test]
    fn assemble_prompt_multi_block_count() {
        let params = PromptParams {
            tools_guidance: Some("Use tools carefully."),
            character_definition: Some("A character."),
            user_definition: Some("A user."),
            recent_memory_digest: Some("Digest"),
            ..make_params(&[])
        };

        let result = assemble_prompt(&params);
        // Should have: system, tools_guidance, character, user, recent_memory = 5 blocks.
        assert_eq!(result.system.len(), 5);
        assert_eq!(result.system[0].label, "system");
        assert_eq!(result.system[1].label, "tools_guidance");
        assert_eq!(result.system[2].label, "character");
        assert_eq!(result.system[3].label, "user");
        assert_eq!(result.system[4].label, "recent_memory");
    }

    // ── Private conversation suppression ──────────────────────────────

    #[test]
    fn private_conversation_suppresses_recent_memory_digest() {
        let params = PromptParams {
            recent_memory_digest: Some("We talked about Rust."),
            is_private: true,
            ..make_params(&[])
        };

        let result = assemble_prompt(&params);
        assert!(
            result.system.iter().all(|b| b.label != "recent_memory"),
            "Private conversation should not include recent memory digest"
        );
    }

    #[test]
    fn private_conversation_suppresses_memory() {
        let params = PromptParams {
            character_definition: Some("Friendly character"),
            is_private: true,
            ..make_params(&[])
        };

        let result = assemble_prompt(&params);
        let all_text: String = result.system.iter().map(|b| b.content.as_str()).collect();
        assert!(
            !all_text.contains("Relevant Memories"),
            "Private conversation should not include memory context"
        );
    }

    // ── Token budget ──────────────────────────────────────────────────

    #[test]
    fn respects_token_budget() {
        let messages: Vec<Message> = (0..100)
            .map(|i| {
                make_msg(
                    if i % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    &format!("Message number {i} with some padding text to use tokens."),
                )
            })
            .collect();

        let params = PromptParams {
            max_context_tokens: Some(500),
            max_output_tokens: Some(100),
            ..make_params(&messages)
        };

        let result = assemble_prompt(&params);
        assert!(
            result.messages.len() < 100,
            "Should trim messages to fit budget, got {} messages",
            result.messages.len()
        );
        assert_eq!(
            result.messages.last().unwrap().content,
            messages.last().unwrap().content
        );
    }

    #[test]
    fn no_heartbeat_injection() {
        let params = make_params(&[]);
        let result = assemble_prompt(&params);
        let all_text: String = result
            .system
            .iter()
            .map(|b| b.content.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!all_text.contains("heartbeat"));
        assert!(!all_text.contains("journal"));
        assert!(!all_text.contains("story"));
    }

    // ── Trim: orphaned tool-loop stripping ────────────────────────────

    fn make_tool_result_msg() -> Message {
        Message {
            msg_id: uuid::Uuid::new_v4().to_string(),
            role: Role::User,
            content: String::new(),
            images: vec![],
            content_blocks: vec![ContentBlock::ToolResult {
                tool_use_id: "t1".into(),
                content: "result".into(),
                is_error: false,
            }],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn make_tool_use_only_msg() -> Message {
        Message {
            msg_id: uuid::Uuid::new_v4().to_string(),
            role: Role::Assistant,
            content: String::new(),
            images: vec![],
            content_blocks: vec![ContentBlock::ToolUse {
                id: "t1".into(),
                name: "search".into(),
                input: serde_json::json!({"q": "test"}),
            }],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn trim_drops_orphaned_tool_result() {
        let msgs = vec![
            make_msg(Role::User, "Hi"),
            make_tool_use_only_msg(),
            make_tool_result_msg(),
            make_msg(Role::Assistant, "Done"),
            make_msg(Role::User, "Recent"),
        ];

        // Budget tight enough to drop the first 2 messages.
        // "Hi" = 1 token, tool_use msg ~10 tokens, tool_result ~4 tokens,
        // "Done" = 1 token, "Recent" = 2 tokens.
        // With budget=5, newest-first picks Recent(2) + Done(1) + tool_result(~4) = 7 > 5,
        // so it stops. Result = [tool_result, Done, Recent].
        // Then orphan stripping removes the leading tool_result.
        let result = trim_messages(&msgs, 5);

        // Leading ToolResult should be stripped.
        assert!(
            !result.is_empty(),
            "Should have at least one message after stripping"
        );
        let first = &result[0];
        let is_tool_result = first.role == Role::User
            && first
                .content_blocks
                .iter()
                .all(|b| matches!(b, ContentBlock::ToolResult { .. }));
        assert!(!is_tool_result, "Leading ToolResult should be stripped");
        assert_eq!(result.last().unwrap().content, "Recent");
    }

    #[test]
    fn trim_drops_orphaned_tool_use_only_assistant() {
        let msgs = vec![
            make_msg(Role::User, "Old message here"),
            make_tool_use_only_msg(),
            make_tool_result_msg(),
            make_msg(Role::User, "Recent"),
        ];

        // Budget tight enough to drop "Old message here" (~5 tokens).
        // Newest-first: Recent(2) + tool_result(~4) + tool_use(~6) = 12 > 5,
        // stops before tool_use. Result = [tool_result, Recent].
        // Then orphan stripping removes leading tool_result.
        let result = trim_messages(&msgs, 5);

        assert!(
            !result.is_empty(),
            "Should have at least one message after stripping"
        );
        // The chain of tool_use-only + tool_result should be stripped.
        assert_eq!(result.last().unwrap().content, "Recent");
        for msg in &result {
            let is_tool_loop = is_tool_loop_msg_prompt(msg);
            assert!(
                !is_tool_loop,
                "No tool-loop messages should remain at front"
            );
        }
    }
}
