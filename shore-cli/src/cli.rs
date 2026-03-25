use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "shore", version, about = "Shore chat client")]
pub struct Cli {
    /// Path to daemon Unix socket (overrides discovery)
    #[arg(long, global = true)]
    pub socket: Option<String>,

    /// Path to config file (selects daemon instance)
    #[arg(long, global = true)]
    pub config: Option<String>,

    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Send a message
    Send {
        /// The message text
        message: Vec<String>,
    },

    /// Regenerate the last assistant response
    Regen {
        /// Optional guidance for the regeneration
        #[arg(short, long)]
        guidance: Option<String>,
    },

    /// Swipe to the next alternative response
    Swipe {
        /// Direction: "next" or "prev"
        #[arg(default_value = "next")]
        direction: String,
    },

    /// Show conversation log
    Log {
        /// Number of messages to show
        #[arg(short, long, default_value = "20")]
        count: u32,
    },

    /// Edit a message by ID
    Edit {
        /// Message ID
        msg_id: String,

        /// New content
        content: Vec<String>,
    },

    /// Delete a message by ID
    Delete {
        /// Message ID
        msg_id: String,
    },

    /// List available characters
    ListCharacters,

    /// Switch to a different character
    SwitchCharacter {
        /// Character name
        name: String,
    },

    /// List conversations
    ListChats,

    /// Switch to a different conversation
    SwitchChat {
        /// Conversation ID
        id: String,
    },

    /// Create a new conversation
    NewChat {
        /// Optional title for the new chat
        #[arg(short, long)]
        title: Option<String>,
    },

    /// Show daemon and session status
    Status,

    /// List available models
    ListModels,

    /// Switch to a different model
    SwitchModel {
        /// Model name
        name: String,
    },

    /// Show or query memory system
    Memory {
        /// Optional query to search memory
        query: Option<String>,
    },

    /// Toggle private mode
    TogglePrivate,

    /// Trigger memory compaction
    Compact,

    /// Toggle autonomous messaging
    ToggleAutonomy,

    /// Show or modify configuration
    Config {
        /// Optional key to get/set
        key: Option<String>,

        /// Value to set (requires key)
        value: Option<String>,
    },
}

/// Map a CLI command to its SWP command name and JSON args.
///
/// Returns `None` for `Send` and `Regen` which use dedicated SWP message types
/// rather than the generic `command` type.
pub fn to_swp_command(cmd: &CliCommand) -> Option<(&'static str, serde_json::Value)> {
    use serde_json::json;
    match cmd {
        // These use dedicated SWP message types, not the command type.
        CliCommand::Send { .. } | CliCommand::Regen { .. } => None,

        CliCommand::Swipe { direction } => {
            Some(("swipe", json!({ "direction": direction })))
        }
        CliCommand::Log { count } => {
            Some(("log", json!({ "count": count })))
        }
        CliCommand::Edit { msg_id, content } => {
            Some(("edit", json!({ "msg_id": msg_id, "content": content.join(" ") })))
        }
        CliCommand::Delete { msg_id } => {
            Some(("delete", json!({ "msg_id": msg_id })))
        }
        CliCommand::ListCharacters => {
            Some(("list_characters", json!({})))
        }
        CliCommand::SwitchCharacter { name } => {
            Some(("switch_character", json!({ "name": name })))
        }
        CliCommand::ListChats => {
            Some(("list_chats", json!({})))
        }
        CliCommand::SwitchChat { id } => {
            Some(("switch_chat", json!({ "id": id })))
        }
        CliCommand::NewChat { title } => {
            Some(("new_chat", json!({ "title": title })))
        }
        CliCommand::Status => {
            Some(("status", json!({})))
        }
        CliCommand::ListModels => {
            Some(("list_models", json!({})))
        }
        CliCommand::SwitchModel { name } => {
            Some(("switch_model", json!({ "name": name })))
        }
        CliCommand::Memory { query } => {
            Some(("memory", json!({ "query": query })))
        }
        CliCommand::TogglePrivate => {
            Some(("toggle_private", json!({})))
        }
        CliCommand::Compact => {
            Some(("compact", json!({})))
        }
        CliCommand::ToggleAutonomy => {
            Some(("toggle_autonomy", json!({})))
        }
        CliCommand::Config { key, value } => {
            Some(("config", json!({ "key": key, "value": value })))
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    /// Helper: parse a command line into a Cli.
    fn parse(args: &[&str]) -> Cli {
        let mut full = vec!["shore"];
        full.extend_from_slice(args);
        Cli::parse_from(full)
    }

    #[test]
    fn parse_send() {
        let cli = parse(&["send", "hello", "world"]);
        match &cli.command {
            CliCommand::Send { message } => {
                assert_eq!(message, &["hello", "world"]);
            }
            other => panic!("expected Send, got: {other:?}"),
        }
    }

    #[test]
    fn parse_regen_no_guidance() {
        let cli = parse(&["regen"]);
        match &cli.command {
            CliCommand::Regen { guidance } => {
                assert!(guidance.is_none());
            }
            other => panic!("expected Regen, got: {other:?}"),
        }
    }

    #[test]
    fn parse_regen_with_guidance() {
        let cli = parse(&["regen", "--guidance", "be more concise"]);
        match &cli.command {
            CliCommand::Regen { guidance } => {
                assert_eq!(guidance.as_deref(), Some("be more concise"));
            }
            other => panic!("expected Regen, got: {other:?}"),
        }
    }

    #[test]
    fn parse_swipe_default_direction() {
        let cli = parse(&["swipe"]);
        match &cli.command {
            CliCommand::Swipe { direction } => {
                assert_eq!(direction, "next");
            }
            other => panic!("expected Swipe, got: {other:?}"),
        }
    }

    #[test]
    fn parse_swipe_prev() {
        let cli = parse(&["swipe", "prev"]);
        match &cli.command {
            CliCommand::Swipe { direction } => {
                assert_eq!(direction, "prev");
            }
            other => panic!("expected Swipe, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_default() {
        let cli = parse(&["log"]);
        match &cli.command {
            CliCommand::Log { count } => {
                assert_eq!(*count, 20);
            }
            other => panic!("expected Log, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_custom_count() {
        let cli = parse(&["log", "--count", "50"]);
        match &cli.command {
            CliCommand::Log { count } => {
                assert_eq!(*count, 50);
            }
            other => panic!("expected Log, got: {other:?}"),
        }
    }

    #[test]
    fn parse_edit() {
        let cli = parse(&["edit", "msg_123", "new", "text"]);
        match &cli.command {
            CliCommand::Edit { msg_id, content } => {
                assert_eq!(msg_id, "msg_123");
                assert_eq!(content, &["new", "text"]);
            }
            other => panic!("expected Edit, got: {other:?}"),
        }
    }

    #[test]
    fn parse_delete() {
        let cli = parse(&["delete", "msg_456"]);
        match &cli.command {
            CliCommand::Delete { msg_id } => {
                assert_eq!(msg_id, "msg_456");
            }
            other => panic!("expected Delete, got: {other:?}"),
        }
    }

    #[test]
    fn parse_list_characters() {
        let cli = parse(&["list-characters"]);
        assert!(matches!(cli.command, CliCommand::ListCharacters));
    }

    #[test]
    fn parse_switch_character() {
        let cli = parse(&["switch-character", "alice"]);
        match &cli.command {
            CliCommand::SwitchCharacter { name } => {
                assert_eq!(name, "alice");
            }
            other => panic!("expected SwitchCharacter, got: {other:?}"),
        }
    }

    #[test]
    fn parse_list_chats() {
        let cli = parse(&["list-chats"]);
        assert!(matches!(cli.command, CliCommand::ListChats));
    }

    #[test]
    fn parse_switch_chat() {
        let cli = parse(&["switch-chat", "conv_001"]);
        match &cli.command {
            CliCommand::SwitchChat { id } => {
                assert_eq!(id, "conv_001");
            }
            other => panic!("expected SwitchChat, got: {other:?}"),
        }
    }

    #[test]
    fn parse_new_chat() {
        let cli = parse(&["new-chat"]);
        match &cli.command {
            CliCommand::NewChat { title } => {
                assert!(title.is_none());
            }
            other => panic!("expected NewChat, got: {other:?}"),
        }
    }

    #[test]
    fn parse_new_chat_with_title() {
        let cli = parse(&["new-chat", "--title", "My Chat"]);
        match &cli.command {
            CliCommand::NewChat { title } => {
                assert_eq!(title.as_deref(), Some("My Chat"));
            }
            other => panic!("expected NewChat, got: {other:?}"),
        }
    }

    #[test]
    fn parse_status() {
        let cli = parse(&["status"]);
        assert!(matches!(cli.command, CliCommand::Status));
    }

    #[test]
    fn parse_list_models() {
        let cli = parse(&["list-models"]);
        assert!(matches!(cli.command, CliCommand::ListModels));
    }

    #[test]
    fn parse_switch_model() {
        let cli = parse(&["switch-model", "claude-haiku-4-5-20251001"]);
        match &cli.command {
            CliCommand::SwitchModel { name } => {
                assert_eq!(name, "claude-haiku-4-5-20251001");
            }
            other => panic!("expected SwitchModel, got: {other:?}"),
        }
    }

    #[test]
    fn parse_memory_no_query() {
        let cli = parse(&["memory"]);
        match &cli.command {
            CliCommand::Memory { query } => {
                assert!(query.is_none());
            }
            other => panic!("expected Memory, got: {other:?}"),
        }
    }

    #[test]
    fn parse_memory_with_query() {
        let cli = parse(&["memory", "recent topics"]);
        match &cli.command {
            CliCommand::Memory { query } => {
                assert_eq!(query.as_deref(), Some("recent topics"));
            }
            other => panic!("expected Memory, got: {other:?}"),
        }
    }

    #[test]
    fn parse_toggle_private() {
        let cli = parse(&["toggle-private"]);
        assert!(matches!(cli.command, CliCommand::TogglePrivate));
    }

    #[test]
    fn parse_compact() {
        let cli = parse(&["compact"]);
        assert!(matches!(cli.command, CliCommand::Compact));
    }

    #[test]
    fn parse_toggle_autonomy() {
        let cli = parse(&["toggle-autonomy"]);
        assert!(matches!(cli.command, CliCommand::ToggleAutonomy));
    }

    #[test]
    fn parse_config_no_args() {
        let cli = parse(&["config"]);
        match &cli.command {
            CliCommand::Config { key, value } => {
                assert!(key.is_none());
                assert!(value.is_none());
            }
            other => panic!("expected Config, got: {other:?}"),
        }
    }

    #[test]
    fn parse_config_with_key() {
        let cli = parse(&["config", "model"]);
        match &cli.command {
            CliCommand::Config { key, value } => {
                assert_eq!(key.as_deref(), Some("model"));
                assert!(value.is_none());
            }
            other => panic!("expected Config, got: {other:?}"),
        }
    }

    #[test]
    fn parse_config_with_key_value() {
        let cli = parse(&["config", "model", "claude-haiku-4-5-20251001"]);
        match &cli.command {
            CliCommand::Config { key, value } => {
                assert_eq!(key.as_deref(), Some("model"));
                assert_eq!(value.as_deref(), Some("claude-haiku-4-5-20251001"));
            }
            other => panic!("expected Config, got: {other:?}"),
        }
    }

    #[test]
    fn parse_global_socket_flag() {
        let cli = parse(&["--socket", "/tmp/shore.sock", "status"]);
        assert_eq!(cli.socket.as_deref(), Some("/tmp/shore.sock"));
        assert!(matches!(cli.command, CliCommand::Status));
    }

    #[test]
    fn parse_global_config_flag() {
        let cli = parse(&["--config", "/etc/shore.toml", "status"]);
        assert_eq!(cli.config.as_deref(), Some("/etc/shore.toml"));
        assert!(matches!(cli.command, CliCommand::Status));
    }

    // ── SWP mapping tests ────────────────────────────────────────────

    #[test]
    fn send_maps_to_none() {
        let cmd = CliCommand::Send {
            message: vec!["hi".into()],
        };
        assert!(to_swp_command(&cmd).is_none());
    }

    #[test]
    fn regen_maps_to_none() {
        let cmd = CliCommand::Regen { guidance: None };
        assert!(to_swp_command(&cmd).is_none());
    }

    #[test]
    fn status_maps_to_command() {
        let cmd = CliCommand::Status;
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "status");
        assert_eq!(args, serde_json::json!({}));
    }

    #[test]
    fn switch_character_maps_to_command() {
        let cmd = CliCommand::SwitchCharacter {
            name: "alice".into(),
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "switch_character");
        assert_eq!(args["name"], "alice");
    }

    #[test]
    fn all_non_message_commands_map() {
        // Every variant except Send and Regen should produce Some.
        let commands: Vec<CliCommand> = vec![
            CliCommand::Swipe { direction: "next".into() },
            CliCommand::Log { count: 20 },
            CliCommand::Edit { msg_id: "m1".into(), content: vec!["text".into()] },
            CliCommand::Delete { msg_id: "m1".into() },
            CliCommand::ListCharacters,
            CliCommand::SwitchCharacter { name: "a".into() },
            CliCommand::ListChats,
            CliCommand::SwitchChat { id: "c1".into() },
            CliCommand::NewChat { title: None },
            CliCommand::Status,
            CliCommand::ListModels,
            CliCommand::SwitchModel { name: "m".into() },
            CliCommand::Memory { query: None },
            CliCommand::TogglePrivate,
            CliCommand::Compact,
            CliCommand::ToggleAutonomy,
            CliCommand::Config { key: None, value: None },
        ];
        for cmd in &commands {
            assert!(
                to_swp_command(cmd).is_some(),
                "expected Some for {cmd:?}"
            );
        }
    }
}
