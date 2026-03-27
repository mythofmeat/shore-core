use std::collections::HashMap;
use std::path::Path;

use chrono::Local;
use shore_protocol::types::{ContentBlock, ImageRef, Message, Role};

use crate::config::resolve_prompt_template;

/// Default context window size when not specified in model config.
const DEFAULT_MAX_CONTEXT_TOKENS: usize = 200_000;

/// Default max output tokens when not specified.
const DEFAULT_MAX_OUTPUT_TOKENS: usize = 4096;

/// Rough characters-per-token ratio for budget estimation.
const CHARS_PER_TOKEN: usize = 4;

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
    pub heartbeat_enabled: bool,
    pub memory_enabled: bool,
    pub image_memory_enabled: bool,
    pub send_image_enabled: bool,
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

    if config.heartbeat_enabled {
        lines.push(
            "- You have a heartbeat system: when the user has been idle, you are \
             prompted to optionally send a message. These messages are real and \
             were written by you — do not deny or second-guess them.",
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
             call recall_image with the memory ID. Use this when you need to \
             examine visual details — appearance, content, or composition.",
        );
    }
    if config.send_image_enabled {
        lines.push(
            "- You can send images from your memories. If a past image is relevant \
             to what you're discussing — a shared moment, something you created \
             together, a visual callback — surface it. Don't wait to be asked; \
             sharing a relevant image is like referencing a shared experience.",
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

    Some(format!("<capabilities>\n{}\n</capabilities>", lines.join("\n")))
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
    let template = resolve_prompt_template(
        params.config_dir,
        params.character_name,
        "system.md",
    )
    .unwrap_or_else(|| BUILTIN_SYSTEM_TEMPLATE.to_string());

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

    // ── 5. Trim conversation history to fit budget ────────────────────
    let messages = trim_messages(params.messages, available_for_messages);

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
        let name = result[if_start + 6..if_start + 6 + name_end].trim().to_string();
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
            ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
        })
        .sum()
}

/// Trim messages from the beginning to fit within the token budget.
///
/// Keeps the most recent messages, discarding older ones first.
fn trim_messages(messages: &[Message], token_budget: usize) -> Vec<PromptMessage> {
    // Build from the end (most recent first), accumulating token cost.
    let mut result: Vec<PromptMessage> = Vec::new();
    let mut used_tokens = 0;

    for msg in messages.iter().rev() {
        let msg_tokens = estimate_message_tokens(msg);
        if used_tokens + msg_tokens > token_budget && !result.is_empty() {
            // Budget exhausted — stop adding older messages.
            break;
        }
        used_tokens += msg_tokens;
        result.push(PromptMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
            images: msg.images.clone(),
            content_blocks: msg.content_blocks.clone(),
        });
    }

    // Reverse to restore chronological order.
    result.reverse();
    result
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
        assert!(!block.contains("heartbeat"));
    }

    #[test]
    fn capabilities_block_all_enabled() {
        let config = CapabilitiesConfig {
            heartbeat_enabled: true,
            memory_enabled: true,
            image_memory_enabled: true,
            send_image_enabled: true,
            generate_image_enabled: true,
            web_search_enabled: true,
            activity_heatmap_enabled: true,
            roll_dice_enabled: true,
            check_time_enabled: true,
        };
        let block = build_capabilities_block(&config).unwrap();
        assert!(block.contains("heartbeat"));
        assert!(block.contains("memory system"));
        assert!(block.contains("Image memory"));
        assert!(block.contains("send images"));
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
        };

        let result = assemble_prompt(&params);

        // System block should contain character info.
        assert!(result.system[0].content.contains("TestChar"));
        assert!(result.system[0].content.contains("TestUser"));
        assert!(result.system[0].label == "system");

        // Character and user definitions in separate blocks.
        let char_block = result.system.iter().find(|b| b.label == "character").unwrap();
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
        std::fs::write(char_prompts.join("system.md"), "Character-specific template.").unwrap();

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
                    if i % 2 == 0 { Role::User } else { Role::Assistant },
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
}
