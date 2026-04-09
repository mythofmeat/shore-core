use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser, Debug)]
#[command(
    name = "shore",
    version,
    about = "Shore chat client",
    disable_help_subcommand = true
)]
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

    /// Disable colored output (also respects NO_COLOR env var)
    #[arg(long, global = true)]
    pub no_color: bool,

    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Subcommand, Debug)]
pub enum CliCommand {
    /// Send a message
    Send {
        /// The message text
        message: Vec<String>,

        /// Attach image file(s) to the message
        #[arg(short = 'i', long = "image")]
        images: Vec<String>,

        /// Override sampling temperature for this message
        #[arg(long)]
        temperature: Option<f64>,

        /// Override nucleus sampling top-p for this message
        #[arg(long)]
        top_p: Option<f64>,

        /// Enable extended thinking with optional budget (tokens)
        #[arg(long, num_args = 0..=1, default_missing_value = "10240")]
        thinking: Option<u32>,

        /// Inject as a system instruction instead of a user message
        #[arg(long)]
        system: bool,
    },

    /// Regenerate the last assistant response
    Regen {
        /// Optional guidance for the regeneration
        #[arg(short, long)]
        guidance: Option<String>,
    },

    /// Show conversation log, get/edit/delete messages
    #[command(args_conflicts_with_subcommands = true)]
    Log {
        #[command(subcommand)]
        subcommand: Option<LogCommand>,

        /// Message reference — show a single message (last, -1, 3, etc.)
        #[arg(allow_hyphen_values = true)]
        msg_ref: Option<String>,

        /// Number of messages to show
        #[arg(short = 'n', long, default_value = "20")]
        count: u32,

        /// Follow mode: keep listening for new messages
        #[arg(short = 'f', long)]
        follow: bool,

        /// Output raw JSON
        #[arg(long)]
        json: bool,

        /// Output only message content (no metadata)
        #[arg(long)]
        content: bool,

        /// Show heartbeat probe decisions and timing history
        #[arg(long)]
        heartbeat: bool,
    },

    /// List or switch characters (no args = list, with name = switch)
    Character {
        /// Character name to switch to
        name: Option<String>,

        /// Show detailed character info
        #[arg(long)]
        info: bool,

        /// Create a new character scaffold directory
        #[arg(long)]
        new: bool,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },

    /// Show daemon and session status
    Status {
        /// Show only a specific section (e.g. autonomy, tokens)
        #[arg(long)]
        section: Option<String>,

        /// Show recent API calls, tool invocations, and errors
        #[arg(long)]
        diagnostics: bool,

        /// Number of diagnostic entries to show (used with --diagnostics)
        #[arg(short = 'n', long, default_value = "10")]
        count: u32,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },

    /// Internal debug utilities (not for normal use)
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        subcommand: DebugCommand,
    },

    /// List or switch models (no args = list, with name = switch)
    Model {
        /// Model name to switch to
        name: Option<String>,

        /// Show detailed model info
        #[arg(long)]
        info: bool,

        /// Reset to config default model
        #[arg(long)]
        reset: bool,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },

    /// Show, query, or manage the memory system
    #[command(args_conflicts_with_subcommands = true)]
    Memory {
        #[command(subcommand)]
        subcommand: Option<MemoryCommand>,

        /// Query to search memory
        query: Option<String>,

        /// Skip the researcher and query the memory agent directly
        #[arg(long)]
        direct: bool,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },

    /// Show or modify configuration
    Config {
        /// Optional key to get/set
        key: Option<String>,

        /// Value to set (requires key)
        value: Option<String>,

        /// Print the config directory path
        #[arg(long)]
        path: bool,

        /// Validate configuration and show warnings
        #[arg(long)]
        check: bool,

        /// Reset all runtime overrides (reload config from disk)
        #[arg(long)]
        reset: bool,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },

    /// Show token usage statistics and costs
    Usage {
        /// Time period: "today", "7d", "30d", "all" (default: today)
        #[arg(long, default_value = "today")]
        last: String,

        /// Filter by character name
        #[arg(long)]
        character: Option<String>,

        /// Filter by provider
        #[arg(long)]
        provider: Option<String>,

        /// Filter by model
        #[arg(long)]
        model: Option<String>,

        /// Filter by call type
        #[arg(long)]
        call_type: Option<String>,

        /// Show only cache anomalies
        #[arg(long)]
        anomalies: bool,

        /// Export full ledger as CSV to stdout
        #[arg(long)]
        export_csv: bool,

        /// Export full ledger as TSV to stdout
        #[arg(long)]
        export_tsv: bool,

        /// Clear cached pricing data
        #[arg(long)]
        refresh_pricing: bool,

        /// Recalculate costs using current pricing
        #[arg(long)]
        recalculate: bool,

        /// Force recalculation of ALL rows (use with --recalculate)
        #[arg(long)]
        force: bool,
    },

    /// Matrix bridge setup and management
    Matrix {
        #[command(subcommand)]
        subcommand: MatrixCommand,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum MatrixCommand {
    /// Initialize embedded Synapse and provision all characters
    Setup,

    /// Register a user account on the embedded Synapse
    Register {
        /// Username (without @ or :server)
        #[arg(long)]
        username: String,

        /// Password (prompted or auto-generated if omitted)
        #[arg(long)]
        password: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum LogCommand {
    /// Edit a message by reference (last, -1, 3, etc.)
    Edit {
        /// Message reference (last, -1, -2, 3, etc.)
        #[arg(allow_hyphen_values = true)]
        msg_ref: String,

        /// New content
        content: Vec<String>,
    },

    /// Delete a message by reference (last, -1, 3, etc.)
    Delete {
        /// Message reference (last, -1, -2, 3, etc.)
        #[arg(allow_hyphen_values = true)]
        msg_ref: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemoryCommand {
    /// Compact conversation into memory entries, then run collation
    Compact,

    /// Show recent memory changelog entries
    Changelog {
        /// Number of entries to show
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
    },

    /// Run memory collation (merge, split, normalize, decay)
    Collate {
        /// Run convergence mode: repeat until no merges/splits occur
        #[arg(long)]
        full: bool,
        /// Override batch limit (max entries to process per run)
        #[arg(long)]
        limit: Option<u64>,
    },

    /// Delete old superseded entries to reclaim space
    Purge {
        /// Minimum age of superseded entries to delete (e.g., 30d, 7d)
        #[arg(long, default_value = "30d")]
        older_than: String,
    },

    /// Rebuild FTS and vector indexes
    Reindex,

    /// Interactive memory agent shell
    Shell,
}

#[derive(Subcommand, Debug)]
pub enum DebugCommand {
    /// Force an interiority tick to fire within ~10 seconds
    ForceTick,
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
        CliCommand::Send { system: false, .. }
        | CliCommand::Regen { .. }
        | CliCommand::Completions { .. }
        | CliCommand::Matrix { .. }
        | CliCommand::Config {
            path: true,
            check: false,
            reset: false,
            ..
        } => None,

        CliCommand::Send {
            system: true,
            message,
            ..
        } => Some(("inject_system", json!({ "text": message.join(" ") }))),

        // Character: list/switch/new handled locally, --info goes to daemon.
        CliCommand::Character { name, info, .. } => {
            if *info {
                let n = name.as_deref().unwrap_or("");
                Some(("character_info", json!({ "name": n })))
            } else {
                // list, switch, and new are handled in run.rs
                None
            }
        }

        // Log: subcommands (edit/delete), single message ref, or list.
        CliCommand::Log {
            subcommand: Some(LogCommand::Edit { msg_ref, content }),
            ..
        } => Some((
            "edit",
            json!({ "ref": msg_ref, "content": content.join(" ") }),
        )),
        CliCommand::Log {
            subcommand: Some(LogCommand::Delete { msg_ref }),
            ..
        } => Some(("delete", json!({ "refs": msg_ref }))),
        CliCommand::Log {
            msg_ref: Some(r), ..
        } => Some(("get", json!({ "ref": r }))),
        CliCommand::Log {
            heartbeat: true,
            count,
            ..
        } => Some(("heartbeat_log", json!({ "count": count }))),
        CliCommand::Log { count, .. } => Some(("log", json!({ "count": count }))),

        // Status: diagnostics mode or normal status.
        CliCommand::Status {
            diagnostics: true,
            count,
            ..
        } => Some(("diagnostics", json!({ "count": count }))),
        CliCommand::Status { .. } => Some(("status", json!({}))),

        CliCommand::Debug { subcommand } => match subcommand {
            DebugCommand::ForceTick => Some(("force_tick", json!({}))),
        },

        CliCommand::Model {
            name, info, reset, ..
        } => {
            if *reset {
                Some(("reset_model", json!({})))
            } else {
                match (name, info) {
                    (Some(name), true) => Some(("model_info", json!({ "name": name }))),
                    (None, true) => Some(("model_info", json!({}))),
                    (None, false) => Some(("list_models", json!({}))),
                    (Some(name), false) => Some(("switch_model", json!({ "name": name }))),
                }
            }
        }

        // Memory: subcommands (compact/changelog/reindex) or status/query.
        CliCommand::Memory {
            subcommand: Some(MemoryCommand::Compact),
            ..
        } => Some(("compact", json!({ "collate": true }))),
        CliCommand::Memory {
            subcommand: Some(MemoryCommand::Collate { full, limit }),
            ..
        } => {
            let mut args = json!({ "full": full });
            if let Some(l) = limit {
                args["limit"] = json!(l);
            }
            Some(("collate", args))
        }
        CliCommand::Memory {
            subcommand: Some(MemoryCommand::Purge { older_than }),
            ..
        } => Some(("memory_purge", json!({ "older_than": older_than }))),
        CliCommand::Memory {
            subcommand: Some(MemoryCommand::Changelog { limit }),
            ..
        } => Some(("memory_changelog", json!({ "limit": limit }))),
        CliCommand::Memory {
            subcommand: Some(MemoryCommand::Reindex),
            ..
        } => Some(("memory_reindex", json!({}))),
        // Shell is handled as a special case in run.rs (interactive REPL).
        CliCommand::Memory {
            subcommand: Some(MemoryCommand::Shell),
            ..
        } => None,
        CliCommand::Memory { query, direct, .. } => {
            Some(("memory", json!({ "query": query, "direct": direct })))
        }

        CliCommand::Config { reset: true, .. } => Some(("config_reset", json!({}))),
        CliCommand::Config { check: true, .. } => Some(("config_check", json!({}))),
        CliCommand::Config { key, value, .. } => {
            Some(("config", json!({ "key": key, "value": value })))
        }

        CliCommand::Usage {
            last,
            character,
            provider,
            model,
            call_type,
            anomalies,
            export_csv,
            export_tsv,
            refresh_pricing,
            recalculate,
            force,
        } => Some((
            "usage",
            json!({
                "last": last,
                "character": character,
                "provider": provider,
                "model": model,
                "call_type": call_type,
                "anomalies": anomalies,
                "export_csv": export_csv,
                "export_tsv": export_tsv,
                "refresh_pricing": refresh_pricing,
                "recalculate": recalculate,
                "force": force,
            }),
        )),
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

    // ── Send ─────────────────────────────────────────────────────────

    #[test]
    fn parse_send() {
        let cli = parse(&["send", "hello", "world"]);
        match &cli.command {
            CliCommand::Send {
                message, images, ..
            } => {
                assert_eq!(message, &["hello", "world"]);
                assert!(images.is_empty());
            }
            other => panic!("expected Send, got: {other:?}"),
        }
    }

    #[test]
    fn parse_send_with_image() {
        let cli = parse(&["send", "-i", "photo.jpg", "describe", "this"]);
        match &cli.command {
            CliCommand::Send {
                message, images, ..
            } => {
                assert_eq!(message, &["describe", "this"]);
                assert_eq!(images, &["photo.jpg"]);
            }
            other => panic!("expected Send, got: {other:?}"),
        }
    }

    #[test]
    fn parse_send_with_multiple_images() {
        let cli = parse(&["send", "-i", "a.jpg", "-i", "b.png", "compare"]);
        match &cli.command {
            CliCommand::Send {
                message, images, ..
            } => {
                assert_eq!(message, &["compare"]);
                assert_eq!(images, &["a.jpg", "b.png"]);
            }
            other => panic!("expected Send, got: {other:?}"),
        }
    }

    // ── Regen ────────────────────────────────────────────────────────

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

    // ── Log ──────────────────────────────────────────────────────────

    #[test]
    fn parse_log_default() {
        let cli = parse(&["log"]);
        match &cli.command {
            CliCommand::Log {
                subcommand,
                msg_ref,
                count,
                follow,
                json,
                content,
                heartbeat,
            } => {
                assert!(subcommand.is_none());
                assert!(msg_ref.is_none());
                assert_eq!(*count, 20);
                assert!(!follow);
                assert!(!json);
                assert!(!content);
                assert!(!heartbeat);
            }
            other => panic!("expected Log, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_custom_count() {
        let cli = parse(&["log", "--count", "50"]);
        match &cli.command {
            CliCommand::Log { count, .. } => {
                assert_eq!(*count, 50);
            }
            other => panic!("expected Log, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_get_by_ref() {
        let cli = parse(&["log", "last"]);
        match &cli.command {
            CliCommand::Log {
                msg_ref,
                subcommand,
                ..
            } => {
                assert!(subcommand.is_none());
                assert_eq!(msg_ref.as_deref(), Some("last"));
            }
            other => panic!("expected Log, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_get_positive_index() {
        let cli = parse(&["log", "3"]);
        match &cli.command {
            CliCommand::Log {
                msg_ref,
                subcommand,
                ..
            } => {
                assert!(subcommand.is_none());
                assert_eq!(msg_ref.as_deref(), Some("3"));
            }
            other => panic!("expected Log, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_edit() {
        let cli = parse(&["log", "edit", "msg_123", "new", "text"]);
        match &cli.command {
            CliCommand::Log {
                subcommand: Some(LogCommand::Edit { msg_ref, content }),
                ..
            } => {
                assert_eq!(msg_ref, "msg_123");
                assert_eq!(content, &["new", "text"]);
            }
            other => panic!("expected Log Edit, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_edit_last() {
        let cli = parse(&["log", "edit", "last", "updated"]);
        match &cli.command {
            CliCommand::Log {
                subcommand: Some(LogCommand::Edit { msg_ref, content }),
                ..
            } => {
                assert_eq!(msg_ref, "last");
                assert_eq!(content, &["updated"]);
            }
            other => panic!("expected Log Edit, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_edit_negative_index() {
        let cli = parse(&["log", "edit", "-1", "new", "text"]);
        match &cli.command {
            CliCommand::Log {
                subcommand: Some(LogCommand::Edit { msg_ref, content }),
                ..
            } => {
                assert_eq!(msg_ref, "-1");
                assert_eq!(content, &["new", "text"]);
            }
            other => panic!("expected Log Edit, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_delete() {
        let cli = parse(&["log", "delete", "msg_456"]);
        match &cli.command {
            CliCommand::Log {
                subcommand: Some(LogCommand::Delete { msg_ref }),
                ..
            } => {
                assert_eq!(msg_ref, "msg_456");
            }
            other => panic!("expected Log Delete, got: {other:?}"),
        }
    }

    #[test]
    fn parse_log_delete_negative_index() {
        let cli = parse(&["log", "delete", "-1"]);
        match &cli.command {
            CliCommand::Log {
                subcommand: Some(LogCommand::Delete { msg_ref }),
                ..
            } => {
                assert_eq!(msg_ref, "-1");
            }
            other => panic!("expected Log Delete, got: {other:?}"),
        }
    }

    // ── Character ────────────────────────────────────────────────────

    #[test]
    fn parse_character_list() {
        let cli = parse(&["character"]);
        match &cli.command {
            CliCommand::Character { name, info, .. } => {
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
            CliCommand::Character { name, info, .. } => {
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
            CliCommand::Character { name, info, .. } => {
                assert_eq!(name.as_deref(), Some("alice"));
                assert!(info);
            }
            other => panic!("expected Character, got: {other:?}"),
        }
    }

    // ── Status ───────────────────────────────────────────────────────

    #[test]
    fn parse_status() {
        let cli = parse(&["status"]);
        match &cli.command {
            CliCommand::Status {
                section,
                diagnostics,
                ..
            } => {
                assert!(section.is_none());
                assert!(!diagnostics);
            }
            other => panic!("expected Status, got: {other:?}"),
        }
    }

    #[test]
    fn parse_status_diagnostics() {
        let cli = parse(&["status", "--diagnostics"]);
        match &cli.command {
            CliCommand::Status {
                diagnostics, count, ..
            } => {
                assert!(diagnostics);
                assert_eq!(*count, 10);
            }
            other => panic!("expected Status, got: {other:?}"),
        }
    }

    #[test]
    fn parse_status_diagnostics_with_count() {
        let cli = parse(&["status", "--diagnostics", "-n", "25"]);
        match &cli.command {
            CliCommand::Status {
                diagnostics, count, ..
            } => {
                assert!(diagnostics);
                assert_eq!(*count, 25);
            }
            other => panic!("expected Status, got: {other:?}"),
        }
    }

    // ── Model ────────────────────────────────────────────────────────

    #[test]
    fn parse_model_list() {
        let cli = parse(&["model"]);
        match &cli.command {
            CliCommand::Model { name, info, .. } => {
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
            CliCommand::Model { name, info, .. } => {
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
            CliCommand::Model { name, info, .. } => {
                assert_eq!(name.as_deref(), Some("opus"));
                assert!(info);
            }
            other => panic!("expected Model, got: {other:?}"),
        }
    }

    // ── Memory ───────────────────────────────────────────────────────

    #[test]
    fn parse_memory_no_query() {
        let cli = parse(&["memory"]);
        match &cli.command {
            CliCommand::Memory {
                query, subcommand, ..
            } => {
                assert!(query.is_none());
                assert!(subcommand.is_none());
            }
            other => panic!("expected Memory, got: {other:?}"),
        }
    }

    #[test]
    fn parse_memory_with_query() {
        let cli = parse(&["memory", "recent topics"]);
        match &cli.command {
            CliCommand::Memory {
                query, subcommand, ..
            } => {
                assert_eq!(query.as_deref(), Some("recent topics"));
                assert!(subcommand.is_none());
            }
            other => panic!("expected Memory, got: {other:?}"),
        }
    }

    #[test]
    fn parse_memory_compact() {
        let cli = parse(&["memory", "compact"]);
        match &cli.command {
            CliCommand::Memory {
                subcommand: Some(MemoryCommand::Compact),
                ..
            } => {}
            other => panic!("expected Memory Compact, got: {other:?}"),
        }
    }

    #[test]
    fn parse_memory_changelog() {
        let cli = parse(&["memory", "changelog"]);
        match &cli.command {
            CliCommand::Memory {
                subcommand: Some(MemoryCommand::Changelog { limit }),
                ..
            } => {
                assert_eq!(*limit, 20);
            }
            other => panic!("expected Memory Changelog, got: {other:?}"),
        }
    }

    #[test]
    fn parse_memory_changelog_with_limit() {
        let cli = parse(&["memory", "changelog", "-n", "50"]);
        match &cli.command {
            CliCommand::Memory {
                subcommand: Some(MemoryCommand::Changelog { limit }),
                ..
            } => {
                assert_eq!(*limit, 50);
            }
            other => panic!("expected Memory Changelog, got: {other:?}"),
        }
    }

    #[test]
    fn parse_memory_reindex() {
        let cli = parse(&["memory", "reindex"]);
        match &cli.command {
            CliCommand::Memory {
                subcommand: Some(MemoryCommand::Reindex),
                ..
            } => {}
            other => panic!("expected Memory Reindex, got: {other:?}"),
        }
    }

    // ── Config ───────────────────────────────────────────────────────

    #[test]
    fn parse_config_no_args() {
        let cli = parse(&["config"]);
        match &cli.command {
            CliCommand::Config {
                key,
                value,
                path,
                check,
                reset,
                ..
            } => {
                assert!(key.is_none());
                assert!(value.is_none());
                assert!(!path);
                assert!(!check);
                assert!(!reset);
            }
            other => panic!("expected Config, got: {other:?}"),
        }
    }

    #[test]
    fn parse_config_with_key() {
        let cli = parse(&["config", "model"]);
        match &cli.command {
            CliCommand::Config { key, value, .. } => {
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
            CliCommand::Config { key, value, .. } => {
                assert_eq!(key.as_deref(), Some("model"));
                assert_eq!(value.as_deref(), Some("claude-haiku-4-5-20251001"));
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

    // ── Global flags ─────────────────────────────────────────────────

    #[test]
    fn parse_global_socket_flag() {
        let cli = parse(&["--socket", "/tmp/shore.sock", "status"]);
        assert_eq!(cli.socket.as_deref(), Some("/tmp/shore.sock"));
        assert!(matches!(cli.command, CliCommand::Status { .. }));
    }

    #[test]
    fn parse_global_config_flag() {
        let cli = parse(&["--config", "/etc/shore.toml", "status"]);
        assert_eq!(cli.config.as_deref(), Some("/etc/shore.toml"));
        assert!(matches!(cli.command, CliCommand::Status { .. }));
    }

    // ── SWP mapping tests ────────────────────────────────────────────

    #[test]
    fn send_maps_to_none() {
        let cmd = CliCommand::Send {
            message: vec!["hi".into()],
            images: vec![],
            temperature: None,
            top_p: None,
            thinking: None,
            system: false,
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
        let cmd = CliCommand::Status {
            section: None,
            diagnostics: false,
            count: 10,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "status");
        assert_eq!(args, serde_json::json!({}));
    }

    #[test]
    fn status_diagnostics_maps_to_command() {
        let cmd = CliCommand::Status {
            section: None,
            diagnostics: true,
            count: 15,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "diagnostics");
        assert_eq!(args["count"], 15);
    }

    #[test]
    fn character_maps_to_none_without_info() {
        let cmd = CliCommand::Character {
            name: None,
            info: false,
            new: false,
            json: false,
        };
        assert!(to_swp_command(&cmd).is_none());
        let cmd = CliCommand::Character {
            name: Some("alice".into()),
            info: false,
            new: false,
            json: false,
        };
        assert!(to_swp_command(&cmd).is_none());
    }

    #[test]
    fn character_info_maps_to_command() {
        let cmd = CliCommand::Character {
            name: Some("alice".into()),
            info: true,
            new: false,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "character_info");
        assert_eq!(args["name"], "alice");
    }

    #[test]
    fn model_info_maps_to_command() {
        let cmd = CliCommand::Model {
            name: Some("opus".into()),
            info: true,
            reset: false,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "model_info");
        assert_eq!(args["name"], "opus");
    }

    #[test]
    fn config_path_maps_to_none() {
        let cmd = CliCommand::Config {
            key: None,
            value: None,
            path: true,
            check: false,
            reset: false,
            json: false,
        };
        assert!(to_swp_command(&cmd).is_none());
    }

    #[test]
    fn log_edit_maps_to_edit_command() {
        let cmd = CliCommand::Log {
            subcommand: Some(LogCommand::Edit {
                msg_ref: "m1".into(),
                content: vec!["new".into(), "text".into()],
            }),
            msg_ref: None,
            count: 20,
            follow: false,
            json: false,
            content: false,
            heartbeat: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "edit");
        assert_eq!(args["ref"], "m1");
        assert_eq!(args["content"], "new text");
    }

    #[test]
    fn log_delete_maps_to_delete_command() {
        let cmd = CliCommand::Log {
            subcommand: Some(LogCommand::Delete {
                msg_ref: "m1".into(),
            }),
            msg_ref: None,
            count: 20,
            follow: false,
            json: false,
            content: false,
            heartbeat: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "delete");
        assert_eq!(args["refs"], "m1");
    }

    #[test]
    fn log_ref_maps_to_get_command() {
        let cmd = CliCommand::Log {
            subcommand: None,
            msg_ref: Some("last".into()),
            count: 20,
            follow: false,
            json: false,
            content: false,
            heartbeat: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "get");
        assert_eq!(args["ref"], "last");
    }

    #[test]
    fn log_default_maps_to_log_command() {
        let cmd = CliCommand::Log {
            subcommand: None,
            msg_ref: None,
            count: 20,
            follow: false,
            json: false,
            content: false,
            heartbeat: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "log");
        assert_eq!(args["count"], 20);
    }

    #[test]
    fn memory_compact_maps_to_compact_command() {
        let cmd = CliCommand::Memory {
            subcommand: Some(MemoryCommand::Compact),
            query: None,
            direct: false,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "compact");
        assert_eq!(args["collate"], true);
    }

    #[test]
    fn memory_changelog_maps_to_command() {
        let cmd = CliCommand::Memory {
            subcommand: Some(MemoryCommand::Changelog { limit: 20 }),
            query: None,
            direct: false,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "memory_changelog");
        assert_eq!(args["limit"], 20);
    }

    #[test]
    fn memory_reindex_maps_to_command() {
        let cmd = CliCommand::Memory {
            subcommand: Some(MemoryCommand::Reindex),
            query: None,
            direct: false,
            json: false,
        };
        let (name, _) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "memory_reindex");
    }

    #[test]
    fn all_non_message_commands_map() {
        // Every variant except Send, Regen, Character (no --info), Config --path, and Completions should produce Some.
        let commands: Vec<CliCommand> = vec![
            CliCommand::Log {
                subcommand: None,
                msg_ref: None,
                count: 20,
                follow: false,
                json: false,
                content: false,
                heartbeat: false,
            },
            CliCommand::Log {
                subcommand: Some(LogCommand::Edit {
                    msg_ref: "m1".into(),
                    content: vec!["text".into()],
                }),
                msg_ref: None,
                count: 20,
                follow: false,
                json: false,
                content: false,
                heartbeat: false,
            },
            CliCommand::Log {
                subcommand: Some(LogCommand::Delete {
                    msg_ref: "m1".into(),
                }),
                msg_ref: None,
                count: 20,
                follow: false,
                json: false,
                content: false,
                heartbeat: false,
            },
            CliCommand::Log {
                subcommand: None,
                msg_ref: Some("last".into()),
                count: 20,
                follow: false,
                json: false,
                content: false,
                heartbeat: false,
            },
            CliCommand::Status {
                section: None,
                diagnostics: false,
                count: 10,
                json: false,
            },
            CliCommand::Status {
                section: None,
                diagnostics: true,
                count: 10,
                json: false,
            },
            CliCommand::Model {
                name: None,
                info: false,
                reset: false,
                json: false,
            },
            CliCommand::Model {
                name: Some("m".into()),
                info: false,
                reset: false,
                json: false,
            },
            CliCommand::Model {
                name: Some("m".into()),
                info: true,
                reset: false,
                json: false,
            },
            CliCommand::Model {
                name: None,
                info: false,
                reset: true,
                json: false,
            },
            CliCommand::Character {
                name: Some("c".into()),
                info: true,
                new: false,
                json: false,
            },
            CliCommand::Memory {
                subcommand: None,
                query: None,
                direct: false,
                json: false,
            },
            CliCommand::Memory {
                subcommand: Some(MemoryCommand::Compact),
                query: None,
                direct: false,
                json: false,
            },
            CliCommand::Memory {
                subcommand: Some(MemoryCommand::Changelog { limit: 20 }),
                query: None,
                direct: false,
                json: false,
            },
            CliCommand::Memory {
                subcommand: Some(MemoryCommand::Reindex),
                query: None,
                direct: false,
                json: false,
            },
            CliCommand::Config {
                key: None,
                value: None,
                path: false,
                check: false,
                reset: false,
                json: false,
            },
        ];
        for cmd in &commands {
            assert!(to_swp_command(cmd).is_some(), "expected Some for {cmd:?}");
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
