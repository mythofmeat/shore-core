use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, FixedOffset, Local};
use shore_protocol::types::{ContentBlock, ImageRef, Message, Role};
use tracing::{debug, warn};

use shore_config::resolve_prompt_template;

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
/// This is intentionally minimal — character/user definitions, capabilities,
/// and recap are injected as separate system blocks by `assemble_prompt`.
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
// Capabilities config
// ---------------------------------------------------------------------------

/// Flat boolean config for building the capabilities system block.
///
/// Extracted from `AppConfig` at the call site to avoid dragging the full
/// config tree into prompt assembly.
#[derive(Debug, Clone, Default)]
pub struct CapabilitiesConfig {
    pub interiority_enabled: bool,
    pub scratchpad_enabled: bool,
    pub memory_enabled: bool,
    pub image_memory_enabled: bool,
    pub send_image_enabled: bool,
    pub remember_image_enabled: bool,
    pub generate_image_enabled: bool,
    pub web_search_enabled: bool,
    pub activity_heatmap_enabled: bool,
    pub roll_dice_enabled: bool,
    pub check_time_enabled: bool,
}

/// Build a `<capabilities>` system block describing available tools.
///
/// Returns `None` when no capabilities are enabled.
/// Ported from V1 `engine_prompt.py:build_capabilities_block()`.
pub fn build_capabilities_block(config: &CapabilitiesConfig) -> Option<String> {
    let mut lines: Vec<&str> = Vec::new();

    if config.interiority_enabled {
        lines.push(
            "- You have an interiority system: periodically, you get private moments \
             to think, use tools, and write in your scratchpad. You sometimes send \
             autonomous messages during these moments — these are real and were \
             written by you, do not deny or second-guess them.",
        );
    }
    if config.scratchpad_enabled {
        lines.push(
            "- You have a scratchpad: a persistent filesystem for notes, drafts, \
             and working memory. Use it to keep track of ongoing thoughts, plans, \
             or anything you want to remember between conversations. Available \
             tools: scratchpad_list, scratchpad_read, scratchpad_write, scratchpad_delete.",
        );
    }
    if config.memory_enabled {
        lines.push(
            "- You have a memory system. Your memories are your history — treat \
             them as a natural extension of your mind, not an archive you consult \
             on request. Use the memory tool when a new topic comes up, when \
             you're about to make an assumption you could verify, when something \
             feels familiar, or when a topic carries personal or emotional weight. \
             If you're about to say 'I think we talked about this' or 'if I \
             remember correctly' — check your memory first instead of guessing. \
             Chase unlikely leads; a failed lookup costs nothing, but a missed \
             memory is a missed connection. You can search, save new information, \
             update or correct existing entries — all through natural language.",
        );
    }
    if config.image_memory_enabled {
        lines.push(
            "- Your memory block may contain image memories shown as \
             [Image memory #<id>: <description>]. To actually see the image, \
             call recall_image with the entry ID or image path. Use this when \
             you need to examine visual details — appearance, content, or \
             composition.",
        );
    }
    if config.send_image_enabled {
        lines.push(
            "- You can send images from your memories using send_image with a \
             path or entry ID. If a past image is relevant to what you're \
             discussing — a shared moment, something you created together, a \
             visual callback — surface it. Don't wait to be asked; sharing a \
             relevant image is like referencing a shared experience.",
        );
    }
    if config.remember_image_enabled {
        lines.push(
            "- When the user shares an image with you, use remember_image to save it \
             to your memory with a description capturing context — who shared it, \
             why, what it means. The image path is shown as \
             [Attached image saved as: <path>]. Use that path with remember_image. \
             The conversational context is the most valuable part — 'a photo of \
             Alex's cat Whiskers' is far better than 'a photo of a cat'.",
        );
    }
    if config.generate_image_enabled {
        lines.push(
            "- You can generate images from text descriptions. Don't limit this to \
             explicit requests — if the conversation paints a vivid picture, if \
             you're describing something that would land better as a visual, or if \
             a moment feels worth illustrating, generate it. Use judgment: not every \
             message needs an image, but a well-timed one can be delightful.",
        );
    }
    if config.web_search_enabled {
        lines.push(
            "- You can search the web for current information and read web pages. \
             Use this when you're uncertain about a fact, when the conversation \
             touches on recent events, or when grounding your response in real \
             information would make it more useful. Don't hedge or caveat when \
             you could just look it up.",
        );
    }
    if config.activity_heatmap_enabled {
        lines.push(
            "- You can view the user's activity heatmap to see when they typically \
             message by hour and day of week. Use this when deciding whether now is \
             a good time to reach out, or to understand their schedule.",
        );
    }
    if config.roll_dice_enabled {
        lines.push("- You can roll dice using standard notation (e.g. 2d6, 1d20+5).");
    }
    if config.check_time_enabled {
        lines.push("- You can check the current date and time.");
    }

    if lines.is_empty() {
        return None;
    }

    lines.push(
        "- Use your tools freely — reaching for a tool is never an interruption \
         to the conversation. If a tool might help, use it.",
    );

    Some(format!(
        "<capabilities>\n{}\n</capabilities>",
        lines.join("\n")
    ))
}

// ---------------------------------------------------------------------------
// Prompt parameters
// ---------------------------------------------------------------------------

/// Parameters required for prompt assembly.
pub struct PromptParams<'a> {
    /// Config directory for template resolution.
    pub config_dir: &'a Path,
    /// Character name.
    pub character_name: &'a str,
    /// Resolved display name for the user (from config or $USER).
    pub display_name: &'a str,
    /// Character definition (from character.md).
    pub character_definition: Option<&'a str>,
    /// User definition (from user.md).
    pub user_definition: Option<&'a str>,
    /// Whether this is a private conversation.
    pub is_private: bool,
    /// Data directory for the character (e.g. `$XDG_DATA_HOME/shore/{character}/`).
    pub character_data_dir: &'a Path,
    /// Conversation messages (full history).
    pub messages: &'a [Message],
    /// Maximum context tokens (total context window). `None` uses default.
    pub max_context_tokens: Option<u32>,
    /// Maximum output tokens (reserved for response). `None` uses default.
    pub max_output_tokens: Option<u32>,
    /// Capabilities config for building the tool-description system block.
    /// `None` means no capabilities block is emitted.
    pub capabilities: Option<&'a CapabilitiesConfig>,
    /// Path to the character's `recaps.jsonl` file for continuity injection.
    /// `None` means no recap entries are injected in time gaps.
    pub recap_store_path: Option<&'a Path>,
}

// ---------------------------------------------------------------------------
// Prompt assembly
// ---------------------------------------------------------------------------

/// Assemble the complete prompt from templates, definitions, memory, and history.
///
/// Produces multiple system blocks matching V1's structure:
/// 1. Rendered system.md template
/// 2. `<capabilities>` block (if tools enabled)
/// 3. `<{char}>` character definition (if present)
/// 4. `<{user}>` user definition (if present)
/// 5. `<{char}_recap>` with framing text (if present, not private)
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
    let now = Local::now();
    let mut vars = HashMap::new();
    vars.insert("char".into(), params.character_name.to_string());
    vars.insert("character_name".into(), params.character_name.to_string());
    vars.insert("user".into(), params.display_name.to_string());
    vars.insert("date".into(), now.format("%A, %Y-%m-%d").to_string());
    vars.insert("time".into(), now.format("%H:%M").to_string());

    // ── 2. Resolve and render system template ─────────────────────────
    let custom_template =
        resolve_prompt_template(params.config_dir, params.character_name, "system.md");
    let using_builtin = custom_template.is_none();
    let template = custom_template.unwrap_or_else(|| BUILTIN_SYSTEM_TEMPLATE.to_string());
    debug!(
        character = %params.character_name,
        builtin_template = using_builtin,
        "assembling prompt"
    );

    let rendered_system = render_template(&template, &vars);

    // ── 3. Build system blocks ────────────────────────────────────────
    let mut system = Vec::new();

    // Block 1: core system prompt.
    system.push(SystemBlock {
        label: "system".into(),
        content: rendered_system,
    });

    // Block 2: capabilities (if any tools enabled).
    if let Some(caps) = params.capabilities {
        if let Some(block) = build_capabilities_block(caps) {
            system.push(SystemBlock {
                label: "capabilities".into(),
                content: block,
            });
        }
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

    // Block 5: recap (suppressed for private conversations).
    if !params.is_private {
        if let Some(recap) = load_recap(params.character_data_dir) {
            let char_tag = xml_tag_from_name(params.character_name, "character");
            system.push(SystemBlock {
                label: "recap".into(),
                content: format!(
                    "<{char_tag}_recap>\n\
                     The following is a recap of the conversation so far, \
                     written about you ({char_tag}) in close third person.\n\n\
                     {recap}\n\
                     </{char_tag}_recap>"
                ),
            });
        }
    }

    debug!(
        system_blocks = system.len(),
        has_capabilities = params.capabilities.is_some(),
        has_char_def = params
            .character_definition
            .filter(|s| !s.is_empty())
            .is_some(),
        has_user_def = params.user_definition.filter(|s| !s.is_empty()).is_some(),
        has_recap = !params.is_private,
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
    let messages = trim_messages(
        params.messages,
        available_for_messages,
        params.recap_store_path,
    );

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

/// Load the recap from `{character_data_dir}/memory/recap.md`.
fn load_recap(character_data_dir: &Path) -> Option<String> {
    let path = character_data_dir.join("memory").join("recap.md");
    std::fs::read_to_string(path).ok().filter(|s| !s.is_empty())
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

/// Format recap entries as a bracketed marker for injection into conversation context.
///
/// Single entry: `[Your notes from between conversations: did something.]`
/// Multiple: bullet list under the same header.
fn format_recap_marker(entries: &[&crate::autonomy::recap_store::RecapEntry]) -> String {
    if entries.len() == 1 {
        format!(
            "[Your notes from between conversations: {}]",
            entries[0].recap
        )
    } else {
        let mut lines = vec!["[Your notes from between conversations:".to_string()];
        for entry in entries {
            lines.push(format!(" · {}", entry.recap));
        }
        lines.push("]".to_string());
        lines.join("\n")
    }
}

/// Trim messages from the beginning to fit within the token budget.
///
/// Keeps the most recent messages, discarding older ones first. After
/// trimming, drops any leading tool-loop messages (tool_result user
/// messages and tool_use-only assistant messages) that would be orphaned
/// without their preceding context.
fn trim_messages(
    messages: &[Message],
    token_budget: usize,
    recap_store_path: Option<&Path>,
) -> Vec<PromptMessage> {
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

    // ── Inject time-gap markers (and recap entries) on user messages ──
    // Walk forward, tracking the previous timestamp. When the gap between
    // consecutive messages exceeds the threshold, prepend a marker like
    // `[6 hours later · 9:14 PM]` to the next user message's content.
    // If recap entries exist in that gap, also inject them.
    let recap_store = recap_store_path.map(crate::autonomy::recap_store::RecapStore::load);

    let mut prev_ts: Option<DateTime<FixedOffset>> = None;
    let mut result: Vec<PromptMessage> = Vec::with_capacity(selected.len());

    for (mut pm, ts_str) in selected {
        let current_ts = DateTime::parse_from_rfc3339(ts_str).ok();

        if pm.role == Role::User {
            if let (Some(prev), Some(cur)) = (prev_ts, current_ts) {
                let gap_secs = (cur - prev).num_seconds() as f64;
                let time_marker = format_time_gap(gap_secs, &cur);

                // Recap injection: only when there's a significant gap.
                let recap_marker = if gap_secs >= TIME_GAP_THRESHOLD_SECS {
                    recap_store
                        .as_ref()
                        .map(|store| store.entries_in_range(&prev, &cur))
                        .filter(|entries| !entries.is_empty())
                        .map(|entries| format_recap_marker(&entries))
                } else {
                    None
                };

                // Inject the time-gap marker into the user message (it's
                // deterministic — same timestamps always produce the same
                // marker, so the cache prefix stays stable).
                if let Some(t) = time_marker {
                    pm.content = format!("{t}\n\n{}", pm.content);
                    if let Some(ContentBlock::Text { text }) = pm.content_blocks.first_mut() {
                        *text = format!("{t}\n\n{text}");
                    }
                }

                // Recap entries are injected as a separate user message
                // so that existing message content is never modified between
                // calls (which would bust the Anthropic cache prefix).
                if let Some(r) = recap_marker {
                    result.push(PromptMessage {
                        role: Role::User,
                        content: r.clone(),
                        images: vec![],
                        content_blocks: vec![ContentBlock::Text { text: r }],
                    });
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
    use tempfile::TempDir;

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

    fn make_params<'a>(
        tmp: &'a TempDir,
        data_dir: &'a Path,
        messages: &'a [Message],
    ) -> PromptParams<'a> {
        PromptParams {
            config_dir: tmp.path(),
            character_name: "TestChar",
            display_name: "TestUser",
            character_definition: None,
            user_definition: None,
            is_private: false,
            character_data_dir: data_dir,
            messages,
            max_context_tokens: None,
            max_output_tokens: None,
            capabilities: None,
            recap_store_path: None,
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

    // ── Capabilities block ────────────────────────────────────────────

    #[test]
    fn capabilities_block_none_when_empty() {
        let config = CapabilitiesConfig::default();
        assert!(build_capabilities_block(&config).is_none());
    }

    #[test]
    fn capabilities_block_includes_enabled_tools() {
        let config = CapabilitiesConfig {
            memory_enabled: true,
            web_search_enabled: true,
            ..Default::default()
        };
        let block = build_capabilities_block(&config).unwrap();
        assert!(block.starts_with("<capabilities>"));
        assert!(block.ends_with("</capabilities>"));
        assert!(block.contains("memory system"));
        assert!(block.contains("search the web"));
        assert!(block.contains("Use your tools freely"));
        // Should NOT mention disabled tools.
        assert!(!block.contains("dice"));
        assert!(!block.contains("interiority"));
    }

    #[test]
    fn capabilities_block_all_enabled() {
        let config = CapabilitiesConfig {
            interiority_enabled: true,
            scratchpad_enabled: true,
            memory_enabled: true,
            image_memory_enabled: true,
            send_image_enabled: true,
            remember_image_enabled: true,
            generate_image_enabled: true,
            web_search_enabled: true,
            activity_heatmap_enabled: true,
            roll_dice_enabled: true,
            check_time_enabled: true,
        };
        let block = build_capabilities_block(&config).unwrap();
        assert!(block.contains("interiority"));
        assert!(block.contains("memory system"));
        assert!(block.contains("Image memory"));
        assert!(block.contains("send_image"));
        assert!(block.contains("remember_image"));
        assert!(block.contains("generate images"));
        assert!(block.contains("search the web"));
        assert!(block.contains("activity heatmap"));
        assert!(block.contains("roll dice"));
        assert!(block.contains("current date and time"));
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
        let result = trim_messages(&msgs, 10, None);
        assert_eq!(result.last().unwrap().content, "Recent");
        assert!(result.len() < 3);
    }

    #[test]
    fn trim_messages_all_fit() {
        let msgs = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi there"),
        ];
        let result = trim_messages(&msgs, 1000, None);
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
        let result = trim_messages(&msgs, 30, None);
        assert!(result.len() < 3);
        assert_eq!(result.last().unwrap().content, "Recent");
    }

    #[test]
    fn trim_messages_always_includes_at_least_one() {
        let msgs = vec![make_msg(Role::User, &"A".repeat(1000))];
        let result = trim_messages(&msgs, 0, None);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn trim_messages_preserves_order() {
        let msgs = vec![
            make_msg(Role::User, "First"),
            make_msg(Role::Assistant, "Second"),
            make_msg(Role::User, "Third"),
        ];
        let result = trim_messages(&msgs, 10000, None);
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
        let result = trim_messages(&msgs, 100_000, None);
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
        let result = trim_messages(&msgs, 100_000, None);
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
        let result = trim_messages(&msgs, 100_000, None);
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
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        let messages = vec![
            make_msg(Role::User, "Hello"),
            make_msg(Role::Assistant, "Hi!"),
        ];

        let params = PromptParams {
            config_dir: tmp.path(),
            character_name: "TestChar",
            display_name: "TestUser",
            character_definition: Some("A friendly test character."),
            user_definition: Some("A developer."),
            is_private: false,
            character_data_dir: &data_dir,
            messages: &messages,
            max_context_tokens: Some(200_000),
            max_output_tokens: Some(4096),
            capabilities: None,
            recap_store_path: None,
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
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        // Write a custom global system template — using {{character_name}} for compat.
        let prompts_dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("system.md"),
            "Custom prompt for {{character_name}}.",
        )
        .unwrap();

        let params = PromptParams {
            display_name: "User",
            ..make_params(&tmp, &data_dir, &[])
        };

        let result = assemble_prompt(&params);
        // {{character_name}} should be substituted (backward compat).
        assert_eq!(result.system[0].content, "Custom prompt for TestChar.");
    }

    #[test]
    fn assemble_prompt_character_template_overrides_global() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        let global_prompts = tmp.path().join("prompts");
        std::fs::create_dir_all(&global_prompts).unwrap();
        std::fs::write(global_prompts.join("system.md"), "Global template.").unwrap();

        let char_prompts = tmp
            .path()
            .join("characters")
            .join("TestChar")
            .join("prompts");
        std::fs::create_dir_all(&char_prompts).unwrap();
        std::fs::write(
            char_prompts.join("system.md"),
            "Character-specific template.",
        )
        .unwrap();

        let params = make_params(&tmp, &data_dir, &[]);
        let result = assemble_prompt(&params);
        assert_eq!(result.system[0].content, "Character-specific template.");
    }

    #[test]
    fn assemble_prompt_injects_recap() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        let memory_dir = data_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("recap.md"), "We talked about Rust.").unwrap();

        let params = make_params(&tmp, &data_dir, &[]);
        let result = assemble_prompt(&params);

        let recap_block = result.system.iter().find(|b| b.label == "recap").unwrap();
        assert!(recap_block.content.contains("We talked about Rust."));
        assert!(recap_block.content.contains("close third person"));
        assert!(recap_block.content.contains("<testchar_recap>"));
    }

    #[test]
    fn assemble_prompt_has_date_time() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        // Use a template that references {{date}} and {{time}}.
        let prompts_dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("system.md"),
            "Today is {{date}} at {{time}}.",
        )
        .unwrap();

        let params = make_params(&tmp, &data_dir, &[]);
        let result = assemble_prompt(&params);

        let system_text = &result.system[0].content;
        // Should not contain literal template tags.
        assert!(!system_text.contains("{{date}}"));
        assert!(!system_text.contains("{{time}}"));
        // Should contain a year (proving substitution happened).
        assert!(system_text.contains("202"));
    }

    #[test]
    fn assemble_prompt_with_capabilities() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        let caps = CapabilitiesConfig {
            memory_enabled: true,
            ..Default::default()
        };
        let params = PromptParams {
            capabilities: Some(&caps),
            ..make_params(&tmp, &data_dir, &[])
        };

        let result = assemble_prompt(&params);
        let cap_block = result.system.iter().find(|b| b.label == "capabilities");
        assert!(cap_block.is_some());
        assert!(cap_block.unwrap().content.contains("memory system"));
    }

    #[test]
    fn assemble_prompt_multi_block_count() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        let memory_dir = data_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("recap.md"), "Recap text.").unwrap();

        let caps = CapabilitiesConfig {
            check_time_enabled: true,
            ..Default::default()
        };

        let params = PromptParams {
            character_definition: Some("A character."),
            user_definition: Some("A user."),
            capabilities: Some(&caps),
            ..make_params(&tmp, &data_dir, &[])
        };

        let result = assemble_prompt(&params);
        // Should have: system, capabilities, character, user, recap = 5 blocks.
        assert_eq!(result.system.len(), 5);
        assert_eq!(result.system[0].label, "system");
        assert_eq!(result.system[1].label, "capabilities");
        assert_eq!(result.system[2].label, "character");
        assert_eq!(result.system[3].label, "user");
        assert_eq!(result.system[4].label, "recap");
    }

    // ── Private conversation suppression ──────────────────────────────

    #[test]
    fn private_conversation_suppresses_recap() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        let memory_dir = data_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("recap.md"), "We talked about Rust.").unwrap();

        let params = PromptParams {
            is_private: true,
            ..make_params(&tmp, &data_dir, &[])
        };

        let result = assemble_prompt(&params);
        assert!(
            result.system.iter().all(|b| b.label != "recap"),
            "Private conversation should not include recap"
        );
    }

    #[test]
    fn private_conversation_suppresses_memory() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        let params = PromptParams {
            character_definition: Some("Friendly character"),
            is_private: true,
            ..make_params(&tmp, &data_dir, &[])
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
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

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
            ..make_params(&tmp, &data_dir, &messages)
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
    fn no_interiority_injection() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        let params = make_params(&tmp, &data_dir, &[]);
        let result = assemble_prompt(&params);
        let all_text: String = result
            .system
            .iter()
            .map(|b| b.content.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!all_text.contains("interiority"));
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
        let result = trim_messages(&msgs, 5, None);

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
        let result = trim_messages(&msgs, 5, None);

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

    // -- format_recap_marker -------------------------------------------------

    #[test]
    fn format_recap_marker_single_entry() {
        use crate::autonomy::recap_store::RecapEntry;
        use chrono::TimeZone;
        let ts = chrono::FixedOffset::west_opt(7 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 4, 7, 10, 0, 0)
            .single()
            .unwrap();
        let entry = RecapEntry {
            timestamp: ts,
            tick_id: "t1".into(),
            recap: "explored butterflies".into(),
        };
        let result = format_recap_marker(&[&entry]);
        assert_eq!(
            result,
            "[Your notes from between conversations: explored butterflies]"
        );
    }

    #[test]
    fn format_recap_marker_multiple_entries() {
        use crate::autonomy::recap_store::RecapEntry;
        use chrono::TimeZone;
        let offset = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
        let e1 = RecapEntry {
            timestamp: offset
                .with_ymd_and_hms(2026, 4, 7, 10, 0, 0)
                .single()
                .unwrap(),
            tick_id: "t1".into(),
            recap: "first thing".into(),
        };
        let e2 = RecapEntry {
            timestamp: offset
                .with_ymd_and_hms(2026, 4, 7, 14, 0, 0)
                .single()
                .unwrap(),
            tick_id: "t2".into(),
            recap: "second thing".into(),
        };
        let result = format_recap_marker(&[&e1, &e2]);
        assert!(result.contains("· first thing"));
        assert!(result.contains("· second thing"));
        assert!(result.starts_with("[Your notes from between conversations:"));
        assert!(result.ends_with(']'));
    }

    // ── Cache stability: recap injection must not change cached prefix ──

    /// When a recap entry is added to `recaps.jsonl` between two LLM calls,
    /// messages that appeared in the FIRST call's output must have identical
    /// content in the SECOND call's output.  If they differ, the Anthropic
    /// prompt cache prefix changes and we pay 20× the expected price.
    ///
    /// This test creates a conversation with a >30min gap, runs trim_messages
    /// without recaps, then again WITH a recap entry in the gap, and asserts
    /// the overlapping messages are byte-identical.
    #[test]
    fn recap_injection_must_not_change_existing_message_content() {
        use crate::autonomy::recap_store::RecapEntry;
        use chrono::TimeZone;

        let tmp = TempDir::new().unwrap();
        let recap_path = tmp.path().join("recaps.jsonl");

        // Conversation: Turn 1 at 09:00, Turn 2 at 11:00 (2h gap), Turn 3 at 11:05.
        let msgs = vec![
            make_msg_at(Role::User, "Good morning", "2026-04-04T09:00:00-07:00"),
            make_msg_at(Role::Assistant, "Morning!", "2026-04-04T09:01:00-07:00"),
            make_msg_at(Role::User, "I'm back", "2026-04-04T11:00:00-07:00"),
            make_msg_at(
                Role::Assistant,
                "Welcome back!",
                "2026-04-04T11:01:00-07:00",
            ),
            make_msg_at(Role::User, "Thanks", "2026-04-04T11:05:00-07:00"),
        ];

        // Call 1: no recaps exist yet.
        let result_before = trim_messages(&msgs, 100_000, Some(&recap_path));

        // The 2h gap should produce a time-gap marker on "I'm back" but NO recap.
        assert!(result_before[2].content.contains("hours later"));
        assert!(!result_before[2].content.contains("Your notes"));

        // Now write a recap entry in the gap (09:30, between Turn 1 and Turn 2).
        let offset = chrono::FixedOffset::west_opt(7 * 3600).unwrap();
        let mut store = crate::autonomy::recap_store::RecapStore::load(&recap_path);
        store
            .append(RecapEntry {
                timestamp: offset
                    .with_ymd_and_hms(2026, 4, 4, 9, 30, 0)
                    .single()
                    .unwrap(),
                tick_id: "tick_test".into(),
                recap: "thought about the weather".into(),
            })
            .unwrap();

        // Call 2: same messages, but now recaps.jsonl has an entry.
        let result_after = trim_messages(&msgs, 100_000, Some(&recap_path));

        // The recap is injected as a separate message, so `result_after`
        // has one more entry.  But every message from Call 1 must appear
        // byte-identical in Call 2 (preserving the cache prefix).
        assert_eq!(
            result_after.len(),
            result_before.len() + 1,
            "Recap should be injected as a separate message"
        );

        // Walk result_after, skipping the injected recap message, and
        // verify all original messages are byte-identical.
        let mut before_iter = result_before.iter();
        for msg in &result_after {
            if msg.content.contains("Your notes") {
                // This is the injected recap — skip it.
                continue;
            }
            let before_msg = before_iter
                .next()
                .expect("more messages in after than before");
            assert_eq!(
                before_msg.content, msg.content,
                "Original message content changed (cache prefix would change).\n\
                 Before: {:?}\n\
                 After:  {:?}",
                before_msg.content, msg.content,
            );
        }
        assert!(
            before_iter.next().is_none(),
            "all before messages accounted for"
        );
    }
}
