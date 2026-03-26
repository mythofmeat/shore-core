use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser, Debug)]
#[command(name = "shore", version, about = "Shore chat client", disable_help_subcommand = true)]
pub struct Cli {
    /// Path to daemon Unix socket (overrides discovery)
    #[arg(long, global = true)]
    pub socket: Option<String>,

    /// Path to config file (selects daemon instance)
    #[arg(long, global = true)]
    pub config: Option<String>,

    /// Character to talk to (overrides SHORE_CHARACTER env var)
    #[arg(long, short = 'c', global = true, env = "SHORE_CHARACTER")]
    pub character: Option<String>,

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

    /// Show conversation log
    Log {
        /// Number of messages to show
        #[arg(short = 'n', long, default_value = "20")]
        count: u32,
    },

    /// Edit a message by ID or relative reference (last, -1, 3, etc.)
    Edit {
        /// Message ID or relative reference (last, -1, -2, 3, etc.)
        #[arg(allow_hyphen_values = true)]
        msg_id: String,

        /// New content
        content: Vec<String>,
    },

    /// Delete a message by ID or relative reference (last, -1, 3, etc.)
    Delete {
        /// Message ID or relative reference (last, -1, -2, 3, etc.)
        #[arg(allow_hyphen_values = true)]
        msg_id: String,
    },

    /// List or switch characters (no args = list, with name = switch)
    Character {
        /// Character name to switch to
        name: Option<String>,

        /// Show detailed character info
        #[arg(long)]
        info: bool,
    },

    /// Show daemon and session status
    Status,

    /// List or switch models (no args = list, with name = switch)
    Model {
        /// Model name to switch to
        name: Option<String>,

        /// Show detailed model info
        #[arg(long)]
        info: bool,
    },

    /// Show or query memory system
    Memory {
        /// Optional query to search memory
        query: Option<String>,
    },

    /// Trigger memory compaction
    Compact,

    /// Show or modify configuration
    Config {
        /// Optional key to get/set
        key: Option<String>,

        /// Value to set (requires key)
        value: Option<String>,

        /// Print the config directory path
        #[arg(long)]
        path: bool,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

/// Generate and print shell completions to stdout.
pub fn print_completions(shell: Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "shore", &mut std::io::stdout());
}

/// Map a CLI command to its SWP command name and JSON args.
///
/// Returns `None` for `Send` and `Regen` which use dedicated SWP message types
/// rather than the generic `command` type.
pub fn to_swp_command(cmd: &CliCommand) -> Option<(&'static str, serde_json::Value)> {
    use serde_json::json;
    match cmd {
        // These use dedicated SWP message types or are handled locally.
        CliCommand::Send { .. }
        | CliCommand::Regen { .. }
        | CliCommand::Completions { .. }
        | CliCommand::Config { path: true, .. } => None,

        // Character: list/switch handled locally, --info goes to daemon.
        CliCommand::Character { name, info } => {
            if *info {
                let n = name.as_deref().unwrap_or("");
                Some(("character_info", json!({ "name": n })))
            } else {
                None
            }
        }

        CliCommand::Log { count } => {
            Some(("log", json!({ "count": count })))
        }
        CliCommand::Edit { msg_id, content } => {
            Some(("edit", json!({ "ref": msg_id, "content": content.join(" ") })))
        }
        CliCommand::Delete { msg_id } => {
            Some(("delete", json!({ "refs": msg_id })))
        }
        CliCommand::Status => {
            Some(("status", json!({})))
        }
        CliCommand::Model { name, info } => match (name, info) {
            (Some(name), true) => Some(("model_info", json!({ "name": name }))),
            (None, true) => Some(("model_info", json!({}))),
            (None, false) => Some(("list_models", json!({}))),
            (Some(name), false) => Some(("switch_model", json!({ "name": name }))),
        }
        CliCommand::Memory { query } => {
            Some(("memory", json!({ "query": query })))
        }
        CliCommand::Compact => {
            Some(("compact", json!({})))
        }
        CliCommand::Config { key, value, .. } => {
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
    fn parse_edit_negative_index() {
        let cli = parse(&["edit", "-1", "new", "text"]);
        match &cli.command {
            CliCommand::Edit { msg_id, content } => {
                assert_eq!(msg_id, "-1");
                assert_eq!(content, &["new", "text"]);
            }
            other => panic!("expected Edit, got: {other:?}"),
        }
    }

    #[test]
    fn parse_delete_negative_index() {
        let cli = parse(&["delete", "-1"]);
        match &cli.command {
            CliCommand::Delete { msg_id } => {
                assert_eq!(msg_id, "-1");
            }
            other => panic!("expected Delete, got: {other:?}"),
        }
    }

    #[test]
    fn parse_edit_last() {
        let cli = parse(&["edit", "last", "updated"]);
        match &cli.command {
            CliCommand::Edit { msg_id, content } => {
                assert_eq!(msg_id, "last");
                assert_eq!(content, &["updated"]);
            }
            other => panic!("expected Edit, got: {other:?}"),
        }
    }

    #[test]
    fn parse_character_list() {
        let cli = parse(&["character"]);
        match &cli.command {
            CliCommand::Character { name, info } => {
                assert!(name.is_none());
                assert!(!info);
            }
            other => panic!("expected Character, got: {other:?}"),
        }
    }

    #[test]
    fn parse_character_switch() {
        let cli = parse(&["character", "alice"]);
        match &cli.command {
            CliCommand::Character { name, info } => {
                assert_eq!(name.as_deref(), Some("alice"));
                assert!(!info);
            }
            other => panic!("expected Character, got: {other:?}"),
        }
    }

    #[test]
    fn parse_character_info() {
        let cli = parse(&["character", "alice", "--info"]);
        match &cli.command {
            CliCommand::Character { name, info } => {
                assert_eq!(name.as_deref(), Some("alice"));
                assert!(info);
            }
            other => panic!("expected Character, got: {other:?}"),
        }
    }

    #[test]
    fn parse_status() {
        let cli = parse(&["status"]);
        assert!(matches!(cli.command, CliCommand::Status));
    }

    #[test]
    fn parse_model_list() {
        let cli = parse(&["model"]);
        match &cli.command {
            CliCommand::Model { name, info } => {
                assert!(name.is_none());
                assert!(!info);
            }
            other => panic!("expected Model, got: {other:?}"),
        }
    }

    #[test]
    fn parse_model_switch() {
        let cli = parse(&["model", "claude-haiku-4-5-20251001"]);
        match &cli.command {
            CliCommand::Model { name, info } => {
                assert_eq!(name.as_deref(), Some("claude-haiku-4-5-20251001"));
                assert!(!info);
            }
            other => panic!("expected Model, got: {other:?}"),
        }
    }

    #[test]
    fn parse_model_info() {
        let cli = parse(&["model", "opus", "--info"]);
        match &cli.command {
            CliCommand::Model { name, info } => {
                assert_eq!(name.as_deref(), Some("opus"));
                assert!(info);
            }
            other => panic!("expected Model, got: {other:?}"),
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
    fn parse_compact() {
        let cli = parse(&["compact"]);
        assert!(matches!(cli.command, CliCommand::Compact));
    }

    #[test]
    fn parse_config_no_args() {
        let cli = parse(&["config"]);
        match &cli.command {
            CliCommand::Config { key, value, path } => {
                assert!(key.is_none());
                assert!(value.is_none());
                assert!(!path);
            }
            other => panic!("expected Config, got: {other:?}"),
        }
    }

    #[test]
    fn parse_config_with_key() {
        let cli = parse(&["config", "model"]);
        match &cli.command {
            CliCommand::Config { key, value, path } => {
                assert_eq!(key.as_deref(), Some("model"));
                assert!(value.is_none());
                assert!(!path);
            }
            other => panic!("expected Config, got: {other:?}"),
        }
    }

    #[test]
    fn parse_config_with_key_value() {
        let cli = parse(&["config", "model", "claude-haiku-4-5-20251001"]);
        match &cli.command {
            CliCommand::Config { key, value, path } => {
                assert_eq!(key.as_deref(), Some("model"));
                assert_eq!(value.as_deref(), Some("claude-haiku-4-5-20251001"));
                assert!(!path);
            }
            other => panic!("expected Config, got: {other:?}"),
        }
    }

    #[test]
    fn parse_config_path() {
        let cli = parse(&["config", "--path"]);
        match &cli.command {
            CliCommand::Config { path, .. } => {
                assert!(path);
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
    fn completions_maps_to_none() {
        let cmd = CliCommand::Completions {
            shell: clap_complete::Shell::Fish,
        };
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
    fn character_maps_to_none_without_info() {
        let cmd = CliCommand::Character { name: None, info: false };
        assert!(to_swp_command(&cmd).is_none());
        let cmd = CliCommand::Character { name: Some("alice".into()), info: false };
        assert!(to_swp_command(&cmd).is_none());
    }

    #[test]
    fn character_info_maps_to_command() {
        let cmd = CliCommand::Character { name: Some("alice".into()), info: true };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "character_info");
        assert_eq!(args["name"], "alice");
    }

    #[test]
    fn model_info_maps_to_command() {
        let cmd = CliCommand::Model { name: Some("opus".into()), info: true };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "model_info");
        assert_eq!(args["name"], "opus");
    }

    #[test]
    fn config_path_maps_to_none() {
        let cmd = CliCommand::Config { key: None, value: None, path: true };
        assert!(to_swp_command(&cmd).is_none());
    }

    #[test]
    fn all_non_message_commands_map() {
        // Every variant except Send, Regen, Character (no --info), Config --path, and Completions should produce Some.
        let commands: Vec<CliCommand> = vec![
            CliCommand::Log { count: 20 },
            CliCommand::Edit { msg_id: "m1".into(), content: vec!["text".into()] },
            CliCommand::Delete { msg_id: "m1".into() },
            CliCommand::Status,
            CliCommand::Model { name: None, info: false },
            CliCommand::Model { name: Some("m".into()), info: false },
            CliCommand::Model { name: Some("m".into()), info: true },
            CliCommand::Character { name: Some("c".into()), info: true },
            CliCommand::Memory { query: None },
            CliCommand::Compact,
            CliCommand::Config { key: None, value: None, path: false },
        ];
        for cmd in &commands {
            assert!(
                to_swp_command(cmd).is_some(),
                "expected Some for {cmd:?}"
            );
        }
    }

    // ── Completions tests ────────────────────────────────────────────

    #[test]
    fn parse_completions_fish() {
        let cli = parse(&["completions", "fish"]);
        match &cli.command {
            CliCommand::Completions { shell } => {
                assert_eq!(*shell, clap_complete::Shell::Fish);
            }
            other => panic!("expected Completions, got: {other:?}"),
        }
    }

    #[test]
    fn parse_completions_bash() {
        let cli = parse(&["completions", "bash"]);
        match &cli.command {
            CliCommand::Completions { shell } => {
                assert_eq!(*shell, clap_complete::Shell::Bash);
            }
            other => panic!("expected Completions, got: {other:?}"),
        }
    }

    #[test]
    fn parse_completions_zsh() {
        let cli = parse(&["completions", "zsh"]);
        match &cli.command {
            CliCommand::Completions { shell } => {
                assert_eq!(*shell, clap_complete::Shell::Zsh);
            }
            other => panic!("expected Completions, got: {other:?}"),
        }
    }

    #[test]
    fn completions_generates_output() {
        // Verify that completion generation produces non-empty output for each shell.
        use clap::CommandFactory;
        for shell in [
            clap_complete::Shell::Fish,
            clap_complete::Shell::Bash,
            clap_complete::Shell::Zsh,
        ] {
            let mut buf = Vec::new();
            clap_complete::generate(shell, &mut Cli::command(), "shore", &mut buf);
            assert!(
                !buf.is_empty(),
                "completions for {shell:?} should produce output"
            );
            let text = String::from_utf8(buf).expect("completions should be valid UTF-8");
            assert!(
                text.contains("shore"),
                "completions for {shell:?} should reference 'shore'"
            );
        }
    }
}
