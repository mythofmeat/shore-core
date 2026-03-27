use std::path::Path;

use shore_protocol::types::{ContentBlock, ImageRef, Message, Role};

use crate::config::resolve_prompt_template;

/// Default context window size when not specified in model config.
const DEFAULT_MAX_CONTEXT_TOKENS: usize = 200_000;

/// Default max output tokens when not specified.
const DEFAULT_MAX_OUTPUT_TOKENS: usize = 4096;

/// Rough characters-per-token ratio for budget estimation.
const CHARS_PER_TOKEN: usize = 4;

/// Built-in system prompt template used when no override is found on disk.
const BUILTIN_SYSTEM_TEMPLATE: &str = r#"You are {{character_name}}, an AI companion.

{{#if character_definition}}
## Character
{{character_definition}}
{{/if}}

{{#if user_definition}}
## User
{{user_definition}}
{{/if}}

{{#if recap}}
## Recap
{{recap}}
{{/if}}"#;

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

/// Parameters required for prompt assembly.
pub struct PromptParams<'a> {
    /// Config directory for template resolution.
    pub config_dir: &'a Path,
    /// Character name.
    pub character_name: &'a str,
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
}

/// Assemble the complete prompt from templates, definitions, memory, and history.
///
/// The pipeline:
/// 1. Resolve system template (character-specific → global → built-in)
/// 2. Render template with character/user definitions
/// 3. Load recap (suppressed for private conversations)
/// 4. Retrieve RAG memory entries (suppressed for private conversations, stubbed)
/// 5. Build system blocks
/// 6. Trim conversation history to fit token budget
pub fn assemble_prompt(params: &PromptParams<'_>) -> AssembledPrompt {
    let max_context = params
        .max_context_tokens
        .map(|t| t as usize)
        .unwrap_or(DEFAULT_MAX_CONTEXT_TOKENS);
    let max_output = params
        .max_output_tokens
        .map(|t| t as usize)
        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);

    // ── 1. Resolve system template ─────────────────────────────────────
    let template = resolve_prompt_template(
        params.config_dir,
        params.character_name,
        "system.md",
    )
    .unwrap_or_else(|| BUILTIN_SYSTEM_TEMPLATE.to_string());

    // ── 2. Render template ─────────────────────────────────────────────
    let recap = if params.is_private {
        None
    } else {
        load_recap(params.character_data_dir)
    };

    let rendered = render_template(
        &template,
        params.character_name,
        params.character_definition,
        params.user_definition,
        recap.as_deref(),
    );

    // ── 3. Build system blocks ─────────────────────────────────────────
    let system = vec![SystemBlock {
        label: "system".to_string(),
        content: rendered,
    }];

    // ── 4. Calculate token budget for messages ─────────────────────────
    let system_tokens = estimate_tokens(&system.iter().map(|b| b.content.as_str()).collect::<Vec<_>>().join("\n"));
    let available_for_messages = max_context.saturating_sub(max_output).saturating_sub(system_tokens);

    // ── 5. Trim conversation history to fit budget ─────────────────────
    let messages = trim_messages(params.messages, available_for_messages);

    AssembledPrompt { system, messages }
}

/// Simple mustache-like template rendering.
///
/// Handles `{{variable}}` substitution and `{{#if var}}...{{/if}}` conditional blocks.
fn render_template(
    template: &str,
    character_name: &str,
    character_definition: Option<&str>,
    user_definition: Option<&str>,
    recap: Option<&str>,
) -> String {
    let mut result = template.to_string();

    // Simple variable substitution.
    result = result.replace("{{character_name}}", character_name);

    // Conditional blocks: {{#if var}}content{{/if}}
    result = render_conditional(&result, "character_definition", character_definition);
    result = render_conditional(&result, "user_definition", user_definition);
    result = render_conditional(&result, "recap", recap);

    result
}

/// Process a `{{#if name}}...{{/if}}` block.
///
/// If `value` is `Some`, replaces the block markers and substitutes `{{name}}`
/// with the value. If `None`, removes the entire block.
fn render_conditional(template: &str, name: &str, value: Option<&str>) -> String {
    let open_tag = format!("{{{{#if {name}}}}}");
    let close_tag = "{{/if}}".to_string();
    let var_tag = format!("{{{{{name}}}}}");

    let Some(start) = template.find(&open_tag) else {
        // No conditional block for this variable — just do variable substitution.
        return match value {
            Some(v) => template.replace(&var_tag, v),
            None => template.replace(&var_tag, ""),
        };
    };

    // Find the matching {{/if}} after the opening tag.
    let after_open = start + open_tag.len();
    let Some(end) = template[after_open..].find(&close_tag) else {
        // Malformed template — return as-is.
        return template.to_string();
    };
    let end_abs = after_open + end;

    let before = &template[..start];
    let block_content = &template[after_open..end_abs];
    let after = &template[end_abs + close_tag.len()..];

    match value {
        Some(v) => {
            let expanded = block_content.replace(&var_tag, v);
            format!("{before}{expanded}{after}")
        }
        None => format!("{before}{after}"),
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
    msg.content_blocks.iter().map(|b| match b {
        ContentBlock::Text { text } => estimate_tokens(text),
        ContentBlock::Thinking { thinking } => estimate_tokens(thinking),
        ContentBlock::ToolUse { input, name, .. } => {
            estimate_tokens(name) + estimate_tokens(&input.to_string())
        }
        ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
    }).sum()
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

    // ── Template rendering ─────────────────────────────────────────────

    #[test]
    fn render_template_substitutes_variables() {
        let template = "Hello, {{character_name}}!";
        let result = render_template(template, "Shore", None, None, None);
        assert_eq!(result, "Hello, Shore!");
    }

    #[test]
    fn render_template_conditional_present() {
        let template = "Start.{{#if character_definition}}\nDef: {{character_definition}}{{/if}}\nEnd.";
        let result = render_template(
            template,
            "Shore",
            Some("A helpful companion"),
            None,
            None,
        );
        assert_eq!(result, "Start.\nDef: A helpful companion\nEnd.");
    }

    #[test]
    fn render_template_conditional_absent() {
        let template = "Start.{{#if character_definition}}\nDef: {{character_definition}}{{/if}}\nEnd.";
        let result = render_template(template, "Shore", None, None, None);
        assert_eq!(result, "Start.\nEnd.");
    }

    #[test]
    fn render_template_all_sections() {
        let result = render_template(
            BUILTIN_SYSTEM_TEMPLATE,
            "TestChar",
            Some("A test character."),
            Some("The test user."),
            Some("Previously, we discussed tests."),
        );
        assert!(result.contains("You are TestChar"));
        assert!(result.contains("A test character."));
        assert!(result.contains("The test user."));
        assert!(result.contains("Previously, we discussed tests."));
    }

    // ── Token estimation ───────────────────────────────────────────────

    #[test]
    fn estimate_tokens_basic() {
        // 12 chars → 3 tokens (12/4)
        assert_eq!(estimate_tokens("Hello world!"), 3);
        // Empty string → 0 tokens
        assert_eq!(estimate_tokens(""), 0);
        // 5 chars → ceil(5/4) = 2
        assert_eq!(estimate_tokens("Hello"), 2);
    }

    // ── Token estimation with content blocks ────────────────────────────

    #[test]
    fn estimate_message_tokens_uses_content_blocks_when_present() {
        let msg = Message {
            msg_id: "m1".into(),
            role: Role::Assistant,
            content: "short".into(), // 5 chars = 2 tokens, but should be ignored
            images: vec![],
            content_blocks: vec![
                ContentBlock::Text { text: "A".repeat(40).to_string() }, // 40 chars = 10 tokens
                ContentBlock::Thinking { thinking: "B".repeat(20).to_string() }, // 20 chars = 5 tokens
            ],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let tokens = estimate_message_tokens(&msg);
        assert_eq!(tokens, 15); // 10 + 5, NOT 2 from content field
    }

    #[test]
    fn estimate_message_tokens_falls_back_to_content_when_no_blocks() {
        let msg = make_msg(Role::User, &"X".repeat(20)); // 20 chars = 5 tokens
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
            content_blocks: vec![
                ContentBlock::ToolUse {
                    id: "tu_1".into(),
                    name: "check_time".into(),
                    input: serde_json::json!({}),
                },
            ],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let tokens = estimate_message_tokens(&msg);
        // name "check_time" (10 chars → 3 tokens) + input "{}" (2 chars → 1 token)
        assert!(tokens > 0);
    }

    #[test]
    fn estimate_message_tokens_tool_result_block() {
        let msg = Message {
            msg_id: "m1".into(),
            role: Role::User,
            content: String::new(),
            images: vec![],
            content_blocks: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "tu_1".into(),
                    content: "2026-03-27T12:00:00Z".into(), // 20 chars = 5 tokens
                    is_error: false,
                },
            ],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let tokens = estimate_message_tokens(&msg);
        assert_eq!(tokens, 5);
    }

    #[test]
    fn trim_messages_accounts_for_content_blocks_size() {
        // A message with large content_blocks should consume more budget.
        let small_msg = make_msg(Role::User, "Hello");
        let big_msg = Message {
            msg_id: "m_big".into(),
            role: Role::Assistant,
            content: String::new(),
            images: vec![],
            content_blocks: vec![
                ContentBlock::Text { text: "X".repeat(400) }, // 400 chars = 100 tokens
            ],
            alt_index: None,
            alt_count: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
        };
        let recent_msg = make_msg(Role::User, "Recent");

        let msgs = vec![small_msg, big_msg, recent_msg];
        // Budget of 10 tokens — big_msg alone is 100 tokens, won't fit alongside others.
        let result = trim_messages(&msgs, 10);
        // Should include at least the most recent, and drop some older ones.
        assert_eq!(result.last().unwrap().content, "Recent");
        assert!(result.len() < 3);
    }

    // ── Message trimming ───────────────────────────────────────────────

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
            make_msg(Role::User, "A".repeat(100).as_str()),
            make_msg(Role::Assistant, "B".repeat(100).as_str()),
            make_msg(Role::User, "Recent"),
        ];
        // Budget for ~30 tokens = 120 chars. The two 100-char messages won't both fit.
        let result = trim_messages(&msgs, 30);
        assert!(result.len() < 3);
        // Most recent message should always be included.
        assert_eq!(result.last().unwrap().content, "Recent");
    }

    #[test]
    fn trim_messages_always_includes_at_least_one() {
        let msgs = vec![make_msg(Role::User, "A".repeat(1000).as_str())];
        // Even with zero budget, we include at least the most recent message.
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

    // ── Full assembly ──────────────────────────────────────────────────

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
            character_definition: Some("A friendly test character."),
            user_definition: Some("A developer."),
            is_private: false,
            character_data_dir: &data_dir,
            messages: &messages,
            max_context_tokens: Some(200_000),
            max_output_tokens: Some(4096),
        };

        let result = assemble_prompt(&params);

        // System block should contain character info.
        assert_eq!(result.system.len(), 1);
        assert!(result.system[0].content.contains("TestChar"));
        assert!(result.system[0].content.contains("A friendly test character."));
        assert!(result.system[0].content.contains("A developer."));

        // Messages should be included.
        assert_eq!(result.messages.len(), 2);
        assert_eq!(result.messages[0].role, Role::User);
        assert_eq!(result.messages[0].content, "Hello");
        assert_eq!(result.messages[1].role, Role::Assistant);
        assert_eq!(result.messages[1].content, "Hi!");
    }

    #[test]
    fn assemble_prompt_uses_custom_template() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        // Write a custom global system template.
        let prompts_dir = tmp.path().join("prompts");
        std::fs::create_dir_all(&prompts_dir).unwrap();
        std::fs::write(
            prompts_dir.join("system.md"),
            "Custom prompt for {{character_name}}.",
        )
        .unwrap();

        let params = PromptParams {
            config_dir: tmp.path(),
            character_name: "Shore",
            character_definition: None,
            user_definition: None,
            is_private: false,
            character_data_dir: &data_dir,
            messages: &[],
            max_context_tokens: None,
            max_output_tokens: None,
        };

        let result = assemble_prompt(&params);
        assert_eq!(result.system[0].content, "Custom prompt for Shore.");
    }

    #[test]
    fn assemble_prompt_character_template_overrides_global() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        // Write both global and character-specific templates.
        let global_prompts = tmp.path().join("prompts");
        std::fs::create_dir_all(&global_prompts).unwrap();
        std::fs::write(
            global_prompts.join("system.md"),
            "Global template.",
        )
        .unwrap();

        let char_prompts = tmp
            .path()
            .join("characters")
            .join("Shore")
            .join("prompts");
        std::fs::create_dir_all(&char_prompts).unwrap();
        std::fs::write(
            char_prompts.join("system.md"),
            "Character-specific template.",
        )
        .unwrap();

        let params = PromptParams {
            config_dir: tmp.path(),
            character_name: "Shore",
            character_definition: None,
            user_definition: None,
            is_private: false,
            character_data_dir: &data_dir,
            messages: &[],
            max_context_tokens: None,
            max_output_tokens: None,
        };

        let result = assemble_prompt(&params);
        assert_eq!(result.system[0].content, "Character-specific template.");
    }

    #[test]
    fn assemble_prompt_injects_recap() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        // Write a recap file.
        let memory_dir = data_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("recap.md"), "We talked about Rust.").unwrap();

        let params = PromptParams {
            config_dir: tmp.path(),
            character_name: "Shore",
            character_definition: None,
            user_definition: None,
            is_private: false,
            character_data_dir: &data_dir,
            messages: &[],
            max_context_tokens: None,
            max_output_tokens: None,
        };

        let result = assemble_prompt(&params);
        assert!(result.system[0].content.contains("We talked about Rust."));
    }

    // ── Private conversation suppression ───────────────────────────────

    #[test]
    fn private_conversation_suppresses_recap() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        // Write a recap file.
        let memory_dir = data_dir.join("memory");
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("recap.md"), "We talked about Rust.").unwrap();

        let params = PromptParams {
            config_dir: tmp.path(),
            character_name: "Shore",
            character_definition: None,
            user_definition: None,
            is_private: true,
            character_data_dir: &data_dir,
            messages: &[],
            max_context_tokens: None,
            max_output_tokens: None,
        };

        let result = assemble_prompt(&params);
        assert!(
            !result.system[0].content.contains("We talked about Rust."),
            "Private conversation should not include recap"
        );
    }

    #[test]
    fn private_conversation_suppresses_memory() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        // Even if RAG returned results (once implemented), private should suppress.
        let params = PromptParams {
            config_dir: tmp.path(),
            character_name: "Shore",
            character_definition: Some("Friendly character"),
            user_definition: None,
            is_private: true,
            character_data_dir: &data_dir,
            messages: &[],
            max_context_tokens: None,
            max_output_tokens: None,
        };

        let result = assemble_prompt(&params);
        // Should not contain memory section header.
        assert!(
            !result.system[0].content.contains("Relevant Memories"),
            "Private conversation should not include memory context"
        );
    }

    // ── Token budget ───────────────────────────────────────────────────

    #[test]
    fn respects_token_budget() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        // Create many messages that exceed a small context window.
        let messages: Vec<Message> = (0..100)
            .map(|i| {
                make_msg(
                    if i % 2 == 0 { Role::User } else { Role::Assistant },
                    &format!("Message number {i} with some padding text to use tokens."),
                )
            })
            .collect();

        let params = PromptParams {
            config_dir: tmp.path(),
            character_name: "Shore",
            character_definition: None,
            user_definition: None,
            is_private: false,
            character_data_dir: &data_dir,
            messages: &messages,
            // Very small context window — should trim significantly.
            max_context_tokens: Some(500),
            max_output_tokens: Some(100),
        };

        let result = assemble_prompt(&params);
        // Should have fewer messages than the full 100.
        assert!(
            result.messages.len() < 100,
            "Should trim messages to fit budget, got {} messages",
            result.messages.len()
        );
        // Most recent message should be present.
        assert_eq!(result.messages.last().unwrap().content, messages.last().unwrap().content);
    }

    #[test]
    fn no_interiority_injection() {
        let tmp = TempDir::new().unwrap();
        let data_dir = tmp.path().join("data");

        let params = PromptParams {
            config_dir: tmp.path(),
            character_name: "Shore",
            character_definition: None,
            user_definition: None,
            is_private: false,
            character_data_dir: &data_dir,
            messages: &[],
            max_context_tokens: None,
            max_output_tokens: None,
        };

        let result = assemble_prompt(&params);
        let system_text = &result.system[0].content;
        // V2 explicitly removes interiority.
        assert!(!system_text.to_lowercase().contains("interiority"));
        assert!(!system_text.to_lowercase().contains("journal"));
        assert!(!system_text.to_lowercase().contains("story"));
    }
}
