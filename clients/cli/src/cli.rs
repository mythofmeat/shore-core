use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

#[derive(Parser, Debug)]
#[command(
    name = "shore",
    version,
    about = "Shore chat client",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// TCP address of the daemon (overrides discovery)
    #[arg(long, global = true)]
    pub addr: Option<String>,

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

    /// Speak a message aloud via TTS, or toggle live-speak mode (on/off)
    Speak {
        /// Message reference (last, -1, 3, etc.) or "on"/"off" for live mode
        #[arg(allow_hyphen_values = true)]
        arg: Option<String>,
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

        /// Plain text output (no colors or decoration), pipe-friendly
        #[arg(long)]
        plain: bool,

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

    /// Advanced debugging utilities
    Debug {
        #[command(subcommand)]
        subcommand: DebugCommand,
    },

    /// List or switch models, or manage saved sampler settings.
    ///
    /// `shore model`                      list visible models
    /// `shore model <name>`               switch active model
    /// `shore model --info [<name>]`      show detailed model info
    /// `shore model --reset`              clear active model selection
    /// `shore model --all`                include hidden discovered models
    /// `shore model setting [...]`        manage saved sampler settings
    #[command(args_conflicts_with_subcommands = true)]
    Model {
        #[command(subcommand)]
        subcommand: Option<ModelCommand>,

        /// Model name to switch to (or look up with --info)
        name: Option<String>,

        /// Show detailed model info
        #[arg(long)]
        info: bool,

        /// Reset to config default model
        #[arg(long)]
        reset: bool,

        /// Include hidden discovered models in the list
        #[arg(long)]
        all: bool,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },

    /// Inspect or manage configured providers.
    ///
    /// `shore provider`                  list providers + key/cache status
    /// `shore provider models <name>`    list discovered + static models
    /// `shore provider refresh <name>`   re-fetch the provider's catalog
    #[command(args_conflicts_with_subcommands = true)]
    Provider {
        #[command(subcommand)]
        subcommand: Option<ProviderCommand>,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },

    /// Show or set the session's reasoning-effort override
    /// (e.g. "low", "medium", "high", or "off" to force no reasoning).
    Reasoning {
        /// Effort value, or "off"/"none" to force reasoning off.
        /// Omit to display the current override + config default.
        value: Option<String>,

        /// Clear the override and revert to the model's configured value
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

        /// Filter by call type. Pass without a value to see a breakdown
        /// grouped by call type (useful for discovering what types exist).
        #[arg(long, num_args = 0..=1)]
        call_type: Option<Option<String>>,

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

    /// Emit plain names for shell completion helpers (internal).
    ///
    /// Used by the dynamic completions appended to `shore completions fish`.
    /// Any failure (daemon down, parse error) results in empty output and
    /// a zero exit code so completions silently fall back to nothing.
    ///
    /// Name kept short (`complete`) rather than something like `__complete`
    /// because clap_complete 4.6.0 panics with
    /// `find_subcommand_with_path` when generating bash completions for
    /// subcommands renamed via `#[command(name = …)]`.
    #[command(hide = true)]
    Complete {
        /// What to enumerate
        kind: CompleteKind,
    },
}

/// Targets for the hidden `__complete` helper.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompleteKind {
    /// Chat model names from the daemon's catalog
    Models,
    /// Discovered character names
    Characters,
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
pub enum ModelCommand {
    /// Show, set, or reset saved sampler settings (temperature, top_p,
    /// reasoning_effort, thinking_enabled, budget_tokens, max_tokens,
    /// cache_ttl) for the active model.
    ///
    /// `shore model setting`                          show effective sampler
    /// `shore model setting <key>`                    show one key
    /// `shore model setting <key> <value>`            set saved value
    /// `shore model setting --reset <key>`            clear saved value
    /// `shore model setting --reset` (no key)         (unsupported — pass a key)
    Setting {
        /// Setting key (temperature, top_p, reasoning_effort, ...)
        key: Option<String>,

        /// Value to assign. For booleans pass true/false.
        /// "off"/"none" map to no-reasoning for `reasoning_effort`.
        value: Option<String>,

        /// Apply to the global preferences file instead of the active
        /// character's. Without this flag, character-scope is used.
        #[arg(long)]
        global: bool,

        /// Clear the saved value for the named key.
        #[arg(long)]
        reset: bool,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProviderCommand {
    /// List discovered + statically configured models for one provider.
    Models {
        /// Provider key (e.g. `openrouter`, `anthropic`)
        name: String,

        /// Include hidden discovered models in the main list
        #[arg(long)]
        all: bool,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },

    /// Re-fetch the provider's `/v1/models` catalog and update the cache.
    Refresh {
        /// Provider key to refresh
        name: String,

        /// Output raw JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemoryCommand {
    /// Compact conversation into markdown memory.
    /// Optional positional: number of recent user turns to retain
    /// (0 = retain none — leaves only the prompt files and memory index).
    Compact { keep_turns: Option<u32> },

    /// Show recent memory changelog entries
    Changelog {
        /// Number of entries to show
        #[arg(short = 'n', long, default_value = "20")]
        limit: u32,
    },

    /// Run or inspect the memory dreaming sweep
    Dream {
        /// Show dreaming scheduler state
        #[arg(long)]
        status: bool,

        /// Preview a sweep without writing dream state, DREAMS.md, or MEMORY.md
        #[arg(long)]
        dry_run: bool,

        /// Run even when the scheduler says the sweep is not due
        #[arg(long)]
        force: bool,
    },

    /// Print recent entries from the dreams audit log
    Dreams {
        /// Maximum number of entries to print (newest first)
        #[arg(short = 'n', long, default_value = "10")]
        limit: u32,
    },
}

#[derive(Subcommand, Debug)]
#[command(rename_all = "snake_case")]
pub enum DebugCommand {
    /// Schedule a heartbeat tick to fire immediately
    #[command(name = "heartbeat_tick_now")]
    TickNow,
    /// Force heartbeat into dormant state (reverts on next user message)
    #[command(name = "heartbeat_status_dormant")]
    StatusDormant,
    /// Force heartbeat into active state (reverts naturally via abandonment guard)
    #[command(name = "heartbeat_status_active")]
    StatusActive,
}

/// Generate and print shell completions to stdout.
///
/// For fish we append dynamic completion lines that shell out to the
/// hidden `shore __complete` helper, so `shore model <TAB>` and
/// `shore character <TAB>` expand to the daemon's live lists instead
/// of leaving the positional argument uncompleted.
pub fn print_completions(shell: Shell) {
    use clap::CommandFactory;
    clap_complete::generate(shell, &mut Cli::command(), "shore", &mut std::io::stdout());
    if shell == Shell::Fish {
        println!("{}", fish_dynamic_completions_footer());
    }
}

/// Fish completions for the positional `name` arguments of `shore model`
/// and `shore character`. Kept as a plain string so unit tests can assert
/// exact content without depending on the clap-generated output above.
pub fn fish_dynamic_completions_footer() -> &'static str {
    // `shore complete <kind> 2>/dev/null` swallows daemon-down errors so
    // fish silently falls back to no suggestions rather than printing a
    // wall of error messages at every tab press.
    "\n\
# ── Dynamic completions (populated by the daemon) ────────────────────\n\
complete -c shore -n \"__fish_shore_using_subcommand model\" -f -a \"(shore complete models 2>/dev/null)\"\n\
complete -c shore -n \"__fish_shore_using_subcommand character\" -f -a \"(shore complete characters 2>/dev/null)\"\n"
}

/// Parse a CLI-supplied sampler value into the JSON shape the daemon
/// expects. The daemon validates types per-key, so this only needs to
/// decide between number/bool/string/null without losing information.
///
/// - `reasoning_effort`: pass through as a string. Synonyms for
///   "disable" ("none"/"disable"/"disabled"/"unset"/"") collapse to the
///   sentinel "off"; the daemon's overlay then explicitly suppresses
///   `reasoning_effort` on the resolved model. JSON null is reserved
///   for *clearing* a saved preference (handled by `unset` flows).
/// - `thinking_enabled`: parse "true"/"false"/"yes"/"no"/"on"/"off".
/// - `temperature`, `top_p`: parse as f64.
/// - `budget_tokens`, `max_tokens`: parse as integer.
/// - `cache_ttl`: pass through as a string.
fn parse_setting_value(key: &str, raw: &str) -> serde_json::Value {
    use serde_json::Value;
    let trimmed = raw.trim();
    match key {
        "thinking_enabled" => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Value::Bool(true),
            "false" | "no" | "off" | "0" => Value::Bool(false),
            _ => Value::String(trimmed.to_string()),
        },
        "temperature" | "top_p" => trimmed
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or_else(|| Value::String(trimmed.to_string())),
        "budget_tokens" | "max_tokens" => trimmed
            .parse::<u64>()
            .map(|n| Value::Number(n.into()))
            .unwrap_or_else(|_| Value::String(trimmed.to_string())),
        "reasoning_effort" => match trimmed.to_ascii_lowercase().as_str() {
            "off" | "none" | "disable" | "disabled" | "unset" | "" => Value::String("off".into()),
            _ => Value::String(trimmed.to_string()),
        },
        _ => Value::String(trimmed.to_string()),
    }
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
        | CliCommand::Speak { .. }
        | CliCommand::Completions { .. }
        | CliCommand::Complete { .. }
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
            DebugCommand::TickNow => Some(("heartbeat_tick_now", json!({}))),
            DebugCommand::StatusDormant => Some(("heartbeat_set_dormant", json!({}))),
            DebugCommand::StatusActive => Some(("heartbeat_set_active", json!({}))),
        },

        CliCommand::Model {
            subcommand:
                Some(ModelCommand::Setting {
                    key,
                    value,
                    global,
                    reset,
                    ..
                }),
            ..
        } => {
            // No key → show current effective sampler. With a key:
            // value=Some → set; --reset → clear; otherwise → show one.
            // The CLI dispatch in run.rs is what actually picks the
            // right daemon command per case; keep this mapping aligned
            // with the "show" path (model_settings).
            match (key.as_deref(), value.as_deref(), *reset) {
                (None, _, _) => Some(("model_settings", json!({}))),
                (Some(k), _, true) => Some((
                    "set_model_setting",
                    json!({
                        "key": k,
                        "value": serde_json::Value::Null,
                        "scope": if *global { "global" } else { "character" },
                    }),
                )),
                (Some(_), None, false) => Some(("model_settings", json!({}))),
                (Some(k), Some(v), false) => Some((
                    "set_model_setting",
                    json!({
                        "key": k,
                        "value": parse_setting_value(k, v),
                        "scope": if *global { "global" } else { "character" },
                    }),
                )),
            }
        }
        CliCommand::Model {
            name,
            info,
            reset,
            all,
            ..
        } => {
            if *reset {
                Some(("reset_model", json!({})))
            } else {
                match (name, info) {
                    (Some(name), true) => Some(("model_info", json!({ "name": name }))),
                    (None, true) => Some(("model_info", json!({}))),
                    (None, false) => {
                        let mut args = json!({});
                        if *all {
                            args["include_hidden"] = json!(true);
                        }
                        Some(("list_models", args))
                    }
                    (Some(name), false) => {
                        let mut args = json!({ "name": name });
                        if *all {
                            args["include_hidden"] = json!(true);
                        }
                        Some(("switch_model", args))
                    }
                }
            }
        }

        CliCommand::Provider {
            subcommand: Some(ProviderCommand::Models { name, all, .. }),
            ..
        } => Some((
            "list_provider_models",
            json!({ "provider": name, "include_hidden": *all }),
        )),
        CliCommand::Provider {
            subcommand: Some(ProviderCommand::Refresh { name, .. }),
            ..
        } => Some(("refresh_provider_models", json!({ "provider": name }))),
        CliCommand::Provider {
            subcommand: None, ..
        } => Some(("list_providers", json!({}))),

        CliCommand::Reasoning { value, reset, .. } => {
            if *reset {
                Some(("set_reasoning_effort", json!({ "clear": true })))
            } else {
                match value.as_deref() {
                    None => Some(("set_reasoning_effort", json!({}))),
                    Some(v) => {
                        // daemon normalises "off"/"none" into null internally
                        Some(("set_reasoning_effort", json!({ "value": v })))
                    }
                }
            }
        }

        // Memory: subcommands (compact/changelog) or status/query.
        CliCommand::Memory {
            subcommand: Some(MemoryCommand::Compact { keep_turns }),
            ..
        } => {
            let mut args = json!({});
            if let Some(n) = keep_turns {
                args["keep_turns"] = json!(n);
            }
            Some(("compact", args))
        }
        CliCommand::Memory {
            subcommand: Some(MemoryCommand::Changelog { limit }),
            ..
        } => Some(("memory_changelog", json!({ "limit": limit }))),
        CliCommand::Memory {
            subcommand:
                Some(MemoryCommand::Dream {
                    status,
                    dry_run,
                    force,
                }),
            ..
        } => Some((
            "memory_dream",
            json!({ "status": status, "dry_run": dry_run, "force": force }),
        )),
        CliCommand::Memory {
            subcommand: Some(MemoryCommand::Dreams { limit }),
            ..
        } => Some(("memory_dreams", json!({ "limit": limit }))),
        CliCommand::Memory { query, .. } => Some(("memory", json!({ "query": query }))),

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
        } => {
            // Three-state flag:
            //   absent         → None,             no grouping, no filter
            //   --call-type    → Some(None),       breakdown mode
            //   --call-type X  → Some(Some("X")),  filter by X
            let (by_call_type, call_type_filter) = match call_type {
                None => (false, None),
                Some(None) => (true, None),
                Some(Some(v)) => (false, Some(v.clone())),
            };
            Some((
                "usage",
                json!({
                    "last": last,
                    "character": character,
                    "provider": provider,
                    "model": model,
                    "call_type": call_type_filter,
                    "by_call_type": by_call_type,
                    "anomalies": anomalies,
                    "export_csv": export_csv,
                    "export_tsv": export_tsv,
                    "refresh_pricing": refresh_pricing,
                    "recalculate": recalculate,
                    "force": force,
                }),
            ))
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
                plain,
                heartbeat,
            } => {
                assert!(subcommand.is_none());
                assert!(msg_ref.is_none());
                assert_eq!(*count, 20);
                assert!(!follow);
                assert!(!json);
                assert!(!content);
                assert!(!plain);
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

    // ── Debug ────────────────────────────────────────────────────────

    #[test]
    fn parse_debug_tick_now() {
        let cli = parse(&["debug", "heartbeat_tick_now"]);
        match &cli.command {
            CliCommand::Debug {
                subcommand: DebugCommand::TickNow,
            } => {}
            other => panic!("expected Debug TickNow, got: {other:?}"),
        }
    }

    #[test]
    fn parse_debug_status_dormant() {
        let cli = parse(&["debug", "heartbeat_status_dormant"]);
        match &cli.command {
            CliCommand::Debug {
                subcommand: DebugCommand::StatusDormant,
            } => {}
            other => panic!("expected Debug StatusDormant, got: {other:?}"),
        }
    }

    #[test]
    fn parse_debug_status_active() {
        let cli = parse(&["debug", "heartbeat_status_active"]);
        match &cli.command {
            CliCommand::Debug {
                subcommand: DebugCommand::StatusActive,
            } => {}
            other => panic!("expected Debug StatusActive, got: {other:?}"),
        }
    }

    // ── Model ────────────────────────────────────────────────────────

    #[test]
    fn parse_model_list() {
        let cli = parse(&["model"]);
        match &cli.command {
            CliCommand::Model {
                name,
                info,
                subcommand,
                all,
                ..
            } => {
                assert!(name.is_none());
                assert!(!info);
                assert!(subcommand.is_none());
                assert!(!all);
            }
            other => panic!("expected Model, got: {other:?}"),
        }
    }

    #[test]
    fn parse_model_switch() {
        let cli = parse(&["model", "claude-haiku-4-5-20251001"]);
        match &cli.command {
            CliCommand::Model {
                name,
                info,
                subcommand,
                ..
            } => {
                assert_eq!(name.as_deref(), Some("claude-haiku-4-5-20251001"));
                assert!(!info);
                assert!(subcommand.is_none());
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

    #[test]
    fn parse_model_all_flag() {
        let cli = parse(&["model", "--all"]);
        match &cli.command {
            CliCommand::Model { all, .. } => assert!(all),
            other => panic!("expected Model, got: {other:?}"),
        }
    }

    #[test]
    fn parse_model_setting_show() {
        let cli = parse(&["model", "setting"]);
        match &cli.command {
            CliCommand::Model {
                subcommand: Some(ModelCommand::Setting { key, value, .. }),
                ..
            } => {
                assert!(key.is_none());
                assert!(value.is_none());
            }
            other => panic!("expected Model Setting, got: {other:?}"),
        }
    }

    #[test]
    fn parse_model_setting_with_value() {
        let cli = parse(&["model", "setting", "temperature", "0.8"]);
        match &cli.command {
            CliCommand::Model {
                subcommand:
                    Some(ModelCommand::Setting {
                        key,
                        value,
                        global,
                        reset,
                        ..
                    }),
                ..
            } => {
                assert_eq!(key.as_deref(), Some("temperature"));
                assert_eq!(value.as_deref(), Some("0.8"));
                assert!(!global);
                assert!(!reset);
            }
            other => panic!("expected Model Setting, got: {other:?}"),
        }
    }

    #[test]
    fn parse_model_setting_reset() {
        let cli = parse(&["model", "setting", "--reset", "temperature"]);
        match &cli.command {
            CliCommand::Model {
                subcommand:
                    Some(ModelCommand::Setting {
                        key, reset, value, ..
                    }),
                ..
            } => {
                assert_eq!(key.as_deref(), Some("temperature"));
                assert!(reset);
                assert!(value.is_none());
            }
            other => panic!("expected Model Setting reset, got: {other:?}"),
        }
    }

    #[test]
    fn parse_model_setting_global_flag() {
        let cli = parse(&["model", "setting", "--global", "top_p", "0.9"]);
        match &cli.command {
            CliCommand::Model {
                subcommand: Some(ModelCommand::Setting { global, .. }),
                ..
            } => {
                assert!(global);
            }
            other => panic!("expected Model Setting global, got: {other:?}"),
        }
    }

    // ── Provider ─────────────────────────────────────────────────────

    #[test]
    fn parse_provider_list() {
        let cli = parse(&["provider"]);
        match &cli.command {
            CliCommand::Provider { subcommand, .. } => assert!(subcommand.is_none()),
            other => panic!("expected Provider, got: {other:?}"),
        }
    }

    #[test]
    fn parse_provider_models() {
        let cli = parse(&["provider", "models", "openrouter"]);
        match &cli.command {
            CliCommand::Provider {
                subcommand: Some(ProviderCommand::Models { name, all, .. }),
                ..
            } => {
                assert_eq!(name, "openrouter");
                assert!(!all);
            }
            other => panic!("expected Provider Models, got: {other:?}"),
        }
    }

    #[test]
    fn parse_provider_models_all() {
        let cli = parse(&["provider", "models", "openrouter", "--all"]);
        match &cli.command {
            CliCommand::Provider {
                subcommand: Some(ProviderCommand::Models { all, .. }),
                ..
            } => assert!(all),
            other => panic!("expected Provider Models, got: {other:?}"),
        }
    }

    #[test]
    fn parse_provider_refresh() {
        let cli = parse(&["provider", "refresh", "openrouter"]);
        match &cli.command {
            CliCommand::Provider {
                subcommand: Some(ProviderCommand::Refresh { name, .. }),
                ..
            } => assert_eq!(name, "openrouter"),
            other => panic!("expected Provider Refresh, got: {other:?}"),
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
                subcommand: Some(MemoryCommand::Compact { keep_turns: None }),
                ..
            } => {}
            other => panic!("expected Memory Compact, got: {other:?}"),
        }
    }

    #[test]
    fn parse_memory_compact_with_keep_turns_zero() {
        let cli = parse(&["memory", "compact", "0"]);
        match &cli.command {
            CliCommand::Memory {
                subcommand:
                    Some(MemoryCommand::Compact {
                        keep_turns: Some(0),
                    }),
                ..
            } => {}
            other => panic!("expected Memory Compact keep_turns=0, got: {other:?}"),
        }
    }

    #[test]
    fn parse_memory_compact_with_keep_turns_n() {
        let cli = parse(&["memory", "compact", "8"]);
        match &cli.command {
            CliCommand::Memory {
                subcommand:
                    Some(MemoryCommand::Compact {
                        keep_turns: Some(8),
                    }),
                ..
            } => {}
            other => panic!("expected Memory Compact keep_turns=8, got: {other:?}"),
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
    fn parse_global_addr_flag() {
        let cli = parse(&["--addr", "127.0.0.1:7320", "status"]);
        assert_eq!(cli.addr.as_deref(), Some("127.0.0.1:7320"));
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
    fn debug_tick_now_maps_to_command() {
        let cmd = CliCommand::Debug {
            subcommand: DebugCommand::TickNow,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "heartbeat_tick_now");
        assert_eq!(args, serde_json::json!({}));
    }

    #[test]
    fn debug_status_dormant_maps_to_command() {
        let cmd = CliCommand::Debug {
            subcommand: DebugCommand::StatusDormant,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "heartbeat_set_dormant");
        assert_eq!(args, serde_json::json!({}));
    }

    #[test]
    fn debug_status_active_maps_to_command() {
        let cmd = CliCommand::Debug {
            subcommand: DebugCommand::StatusActive,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "heartbeat_set_active");
        assert_eq!(args, serde_json::json!({}));
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
            subcommand: None,
            name: Some("opus".into()),
            info: true,
            reset: false,
            all: false,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "model_info");
        assert_eq!(args["name"], "opus");
    }

    #[test]
    fn model_list_with_all_includes_hidden_arg() {
        let cmd = CliCommand::Model {
            subcommand: None,
            name: None,
            info: false,
            reset: false,
            all: true,
            json: false,
        };
        let (cmd_name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(cmd_name, "list_models");
        assert_eq!(args["include_hidden"], true);
    }

    #[test]
    fn model_setting_no_key_maps_to_show() {
        let cmd = CliCommand::Model {
            subcommand: Some(ModelCommand::Setting {
                key: None,
                value: None,
                global: false,
                reset: false,
                json: false,
            }),
            name: None,
            info: false,
            reset: false,
            all: false,
            json: false,
        };
        let (name, _) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "model_settings");
    }

    #[test]
    fn model_setting_with_value_maps_to_set_model_setting() {
        let cmd = CliCommand::Model {
            subcommand: Some(ModelCommand::Setting {
                key: Some("temperature".into()),
                value: Some("0.8".into()),
                global: false,
                reset: false,
                json: false,
            }),
            name: None,
            info: false,
            reset: false,
            all: false,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "set_model_setting");
        assert_eq!(args["key"], "temperature");
        assert_eq!(args["value"], 0.8);
        assert_eq!(args["scope"], "character");
    }

    #[test]
    fn model_setting_reset_clears_with_null_value() {
        let cmd = CliCommand::Model {
            subcommand: Some(ModelCommand::Setting {
                key: Some("budget_tokens".into()),
                value: None,
                global: false,
                reset: true,
                json: false,
            }),
            name: None,
            info: false,
            reset: false,
            all: false,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "set_model_setting");
        assert!(args["value"].is_null());
    }

    #[test]
    fn model_setting_global_scope_routes_correctly() {
        let cmd = CliCommand::Model {
            subcommand: Some(ModelCommand::Setting {
                key: Some("top_p".into()),
                value: Some("0.95".into()),
                global: true,
                reset: false,
                json: false,
            }),
            name: None,
            info: false,
            reset: false,
            all: false,
            json: false,
        };
        let (_, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(args["scope"], "global");
    }

    #[test]
    fn model_setting_reasoning_off_sends_off_sentinel() {
        // `shore model setting reasoning_effort off` must send the
        // string "off" (not JSON null). The daemon's overlay maps
        // Some("off") → unset reasoning_effort on the resolved model,
        // while null clears the saved override and lets the model's
        // intrinsic value leak through.
        let cmd = CliCommand::Model {
            subcommand: Some(ModelCommand::Setting {
                key: Some("reasoning_effort".into()),
                value: Some("off".into()),
                global: false,
                reset: false,
                json: false,
            }),
            name: None,
            info: false,
            reset: false,
            all: false,
            json: false,
        };
        let (_, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(args["value"], "off");
    }

    #[test]
    fn model_setting_reasoning_disable_synonyms_normalize_to_off() {
        for synonym in ["none", "DISABLE", "Disabled", "unset", ""] {
            let cmd = CliCommand::Model {
                subcommand: Some(ModelCommand::Setting {
                    key: Some("reasoning_effort".into()),
                    value: Some(synonym.into()),
                    global: false,
                    reset: false,
                    json: false,
                }),
                name: None,
                info: false,
                reset: false,
                all: false,
                json: false,
            };
            let (_, args) = to_swp_command(&cmd).unwrap();
            assert_eq!(args["value"], "off", "synonym {synonym:?}");
        }
    }

    #[test]
    fn provider_no_subcommand_lists_providers() {
        let cmd = CliCommand::Provider {
            subcommand: None,
            json: false,
        };
        let (name, _) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "list_providers");
    }

    #[test]
    fn provider_models_maps_to_command() {
        let cmd = CliCommand::Provider {
            subcommand: Some(ProviderCommand::Models {
                name: "openrouter".into(),
                all: true,
                json: false,
            }),
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "list_provider_models");
        assert_eq!(args["provider"], "openrouter");
        assert_eq!(args["include_hidden"], true);
    }

    #[test]
    fn provider_refresh_maps_to_command() {
        let cmd = CliCommand::Provider {
            subcommand: Some(ProviderCommand::Refresh {
                name: "openrouter".into(),
                json: false,
            }),
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "refresh_provider_models");
        assert_eq!(args["provider"], "openrouter");
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
            plain: false,
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
            plain: false,
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
            plain: false,
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
            plain: false,
            heartbeat: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "log");
        assert_eq!(args["count"], 20);
    }

    #[test]
    fn memory_compact_maps_to_compact_command() {
        let cmd = CliCommand::Memory {
            subcommand: Some(MemoryCommand::Compact { keep_turns: None }),
            query: None,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "compact");
        assert!(args.get("keep_turns").is_none());
    }

    #[test]
    fn memory_compact_with_keep_turns_includes_field() {
        let cmd = CliCommand::Memory {
            subcommand: Some(MemoryCommand::Compact {
                keep_turns: Some(0),
            }),
            query: None,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "compact");
        assert_eq!(args["keep_turns"], 0);
    }

    #[test]
    fn memory_changelog_maps_to_command() {
        let cmd = CliCommand::Memory {
            subcommand: Some(MemoryCommand::Changelog { limit: 20 }),
            query: None,
            json: false,
        };
        let (name, args) = to_swp_command(&cmd).unwrap();
        assert_eq!(name, "memory_changelog");
        assert_eq!(args["limit"], 20);
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
                plain: false,
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
                plain: false,
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
                plain: false,
                heartbeat: false,
            },
            CliCommand::Log {
                subcommand: None,
                msg_ref: Some("last".into()),
                count: 20,
                follow: false,
                json: false,
                content: false,
                plain: false,
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
            CliCommand::Debug {
                subcommand: DebugCommand::TickNow,
            },
            CliCommand::Debug {
                subcommand: DebugCommand::StatusDormant,
            },
            CliCommand::Debug {
                subcommand: DebugCommand::StatusActive,
            },
            CliCommand::Model {
                subcommand: None,
                name: None,
                info: false,
                reset: false,
                all: false,
                json: false,
            },
            CliCommand::Model {
                subcommand: None,
                name: Some("m".into()),
                info: false,
                reset: false,
                all: false,
                json: false,
            },
            CliCommand::Model {
                subcommand: None,
                name: Some("m".into()),
                info: true,
                reset: false,
                all: false,
                json: false,
            },
            CliCommand::Model {
                subcommand: None,
                name: None,
                info: false,
                reset: true,
                all: false,
                json: false,
            },
            CliCommand::Model {
                subcommand: Some(ModelCommand::Setting {
                    key: None,
                    value: None,
                    global: false,
                    reset: false,
                    json: false,
                }),
                name: None,
                info: false,
                reset: false,
                all: false,
                json: false,
            },
            CliCommand::Provider {
                subcommand: None,
                json: false,
            },
            CliCommand::Provider {
                subcommand: Some(ProviderCommand::Models {
                    name: "openrouter".into(),
                    all: false,
                    json: false,
                }),
                json: false,
            },
            CliCommand::Provider {
                subcommand: Some(ProviderCommand::Refresh {
                    name: "openrouter".into(),
                    json: false,
                }),
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
                json: false,
            },
            CliCommand::Memory {
                subcommand: Some(MemoryCommand::Compact { keep_turns: None }),
                query: None,
                json: false,
            },
            CliCommand::Memory {
                subcommand: Some(MemoryCommand::Changelog { limit: 20 }),
                query: None,
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

    // ── Usage ────────────────────────────────────────────────────────

    #[test]
    fn parse_usage_no_call_type_flag() {
        let cli = parse(&["usage"]);
        match &cli.command {
            CliCommand::Usage { call_type, .. } => {
                assert!(call_type.is_none(), "flag absent → None");
            }
            other => panic!("expected Usage, got: {other:?}"),
        }
    }

    #[test]
    fn parse_usage_bare_call_type_flag() {
        // Regression: `shore usage --call-type` previously errored because
        // clap required a value. The bare flag should mean "break down by
        // call type" (Some(None)).
        let cli = parse(&["usage", "--call-type"]);
        match &cli.command {
            CliCommand::Usage { call_type, .. } => {
                assert_eq!(*call_type, Some(None), "bare flag → Some(None)");
            }
            other => panic!("expected Usage, got: {other:?}"),
        }
    }

    #[test]
    fn parse_usage_call_type_with_value() {
        let cli = parse(&["usage", "--call-type", "message"]);
        match &cli.command {
            CliCommand::Usage { call_type, .. } => {
                assert_eq!(*call_type, Some(Some("message".into())));
            }
            other => panic!("expected Usage, got: {other:?}"),
        }
    }

    #[test]
    fn usage_bare_call_type_sets_by_call_type_flag() {
        // Wire-level: daemon should see `by_call_type: true` and no
        // `call_type` filter when the user passed the bare flag.
        let cli = parse(&["usage", "--call-type"]);
        let (cmd, args) = to_swp_command(&cli.command).unwrap();
        assert_eq!(cmd, "usage");
        assert_eq!(args["by_call_type"], serde_json::Value::Bool(true));
        assert!(args["call_type"].is_null());
    }

    #[test]
    fn usage_call_type_value_sets_filter_not_flag() {
        let cli = parse(&["usage", "--call-type", "message"]);
        let (_cmd, args) = to_swp_command(&cli.command).unwrap();
        assert_eq!(args["call_type"], "message");
        assert!(
            args["by_call_type"].is_null()
                || args["by_call_type"] == serde_json::Value::Bool(false),
            "filter value should not imply breakdown flag",
        );
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

    // ── Dynamic completions (regression #3 followup) ────────────────

    #[test]
    fn fish_footer_has_dynamic_lines_for_model_and_character() {
        let footer = fish_dynamic_completions_footer();
        // Each line must bind the positional for its subcommand to
        // the `complete` helper — anything else means fish will
        // silently not expand `shore model <TAB>` again.
        assert!(
            footer.contains("__fish_shore_using_subcommand model"),
            "footer must gate the model completion on the model subcommand",
        );
        assert!(
            footer.contains("shore complete models"),
            "footer must shell out to `shore complete models`",
        );
        assert!(
            footer.contains("__fish_shore_using_subcommand character"),
            "footer must gate the character completion on the character subcommand",
        );
        assert!(
            footer.contains("shore complete characters"),
            "footer must shell out to `shore complete characters`",
        );
        // Errors from `complete` (daemon down, stale registry) must
        // not propagate to fish, or the user sees red at every tab.
        assert!(
            footer.contains("2>/dev/null"),
            "footer must swallow stderr from the helper",
        );
    }

    #[test]
    fn parse_complete_models() {
        // The `complete` helper must parse cleanly so fish can call
        // it at completion time without triggering a clap error.
        let cli = parse(&["complete", "models"]);
        match &cli.command {
            CliCommand::Complete { kind } => {
                assert_eq!(*kind, CompleteKind::Models);
            }
            other => panic!("expected Complete, got: {other:?}"),
        }
    }

    #[test]
    fn parse_complete_characters() {
        let cli = parse(&["complete", "characters"]);
        match &cli.command {
            CliCommand::Complete { kind } => {
                assert_eq!(*kind, CompleteKind::Characters);
            }
            other => panic!("expected Complete, got: {other:?}"),
        }
    }

    #[test]
    fn complete_maps_to_none_swp() {
        // `complete` is handled entirely client-side; it must not leak
        // into the generic SWP dispatch path.
        let cmd = CliCommand::Complete {
            kind: CompleteKind::Models,
        };
        assert!(
            to_swp_command(&cmd).is_none(),
            "complete is a client-side helper, not an SWP command",
        );
    }
}
