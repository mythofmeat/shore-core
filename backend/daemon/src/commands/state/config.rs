use serde_json::json;
use shore_config::models::Sdk;
use shore_protocol::error::ErrorCode;
use tokio::process::Command;
use tracing::info;

use crate::commands::{CommandContext, CommandResult};

/// Validate configuration and return warnings/info.
pub async fn config_check(ctx: &CommandContext) -> CommandResult {
    let mut warnings: Vec<String> = Vec::new();
    let mut info: Vec<String> = Vec::new();

    // Check: any chat models configured?
    if ctx.config.models.chat.is_empty() {
        warnings.push("No chat models configured. Add [chat.*] sections to config.".into());
    } else {
        info.push(format!(
            "{} chat model(s) configured",
            ctx.config.models.chat.len()
        ));
    }

    // Check: default model set?
    match &ctx.config.app.defaults.model {
        Some(m) => {
            if ctx.config.models.find_model(m).is_ok() {
                info.push(format!("Default model: {m}"));
            } else {
                warnings.push(format!("Default model \"{m}\" not found in catalog"));
            }
        }
        None => {
            if !ctx.config.models.chat.is_empty() {
                warnings.push("No default model set. First chat model will be used.".into());
            }
        }
    }

    // Check: tool models
    if ctx.config.models.tools.is_empty() {
        info.push("No tool models configured (chat models will be used for tools)".into());
    } else {
        info.push(format!(
            "{} tool model(s) configured",
            ctx.config.models.tools.len()
        ));
    }

    // Check: LLM service configured?
    if ctx.config.app.services.llm.command.is_none() && ctx.config.app.services.llm.socket.is_none()
    {
        warnings.push(
            "No LLM service configured. Set [services.llm].command or [services.llm].socket."
                .into(),
        );
    }

    // Check: API key env vars are set for configured providers
    for model in ctx.config.models.chat.values() {
        if let Some(ref key_env) = model.api_key_env {
            if std::env::var(key_env).is_err() {
                warnings.push(format!(
                    "API key env var ${} not set (needed by model {})",
                    key_env, model.qualified_name
                ));
            }
        }
    }

    let claude_code_models: Vec<_> = ctx
        .config
        .models
        .chat
        .values()
        .filter(|model| model.sdk == Sdk::ClaudeCode)
        .collect();
    if !claude_code_models.is_empty() {
        info.push(format!(
            "Claude Code provider configured for {} chat model(s)",
            claude_code_models.len()
        ));

        if !ctx.config.app.daemon.http.enabled {
            warnings.push(
                "Claude Code models require [daemon.http].enabled = true so the local claude CLI can reach shore's MCP listener."
                    .into(),
            );
        }

        match which::which("claude") {
            Ok(path) => {
                info.push(format!("claude CLI found at {}", path.display()));
                match Command::new(&path)
                    .args(["auth", "status", "--json"])
                    .output()
                    .await
                {
                    Ok(output) if output.status.success() => {
                        match serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                            Ok(status) => {
                                let logged_in = status
                                    .get("loggedIn")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let auth_method =
                                    status.get("authMethod").and_then(|v| v.as_str());
                                if !logged_in {
                                    warnings.push(
                                        "claude CLI is installed but is not logged in; run `claude auth login`."
                                            .into(),
                                    );
                                } else if auth_method != Some("claude.ai") {
                                    warnings.push(
                                        "claude CLI is logged in without claude.ai OAuth; run `claude auth login` for Max subscription usage."
                                            .into(),
                                    );
                                } else {
                                    let subscription = status
                                        .get("subscriptionType")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("unknown");
                                    info.push(format!(
                                        "claude CLI OAuth login detected (subscription: {subscription})"
                                    ));
                                }
                            }
                            Err(e) => warnings.push(format!(
                                "Could not parse `claude auth status --json` output: {e}"
                            )),
                        }
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        warnings.push(format!(
                            "`claude auth status --json` failed; run `claude auth login` ({})",
                            stderr.trim()
                        ));
                    }
                    Err(e) => warnings.push(format!(
                        "Could not run `claude auth status --json`: {e}"
                    )),
                }
            }
            Err(_) => warnings.push(
                "Claude Code models require the `claude` CLI on PATH; install Claude Code and run `claude auth login`."
                    .into(),
            ),
        }
    }

    let valid = warnings.is_empty();

    Ok(json!({
        "valid": valid,
        "warnings": warnings,
        "info": info,
        "config_dir": ctx.config.dirs.config.display().to_string(),
        "data_dir": ctx.config.dirs.data.display().to_string(),
        "cache_dir": ctx.config.dirs.cache.display().to_string(),
        "chat_models": ctx.config.models.chat.len(),
        "tool_models": ctx.config.models.tools.len(),
        "memory_mode": "markdown",
    }))
}

pub fn config(ctx: &mut CommandContext, args: &serde_json::Value) -> CommandResult {
    let key = args.get("key").and_then(|v| v.as_str());
    let value = args.get("value").and_then(|v| v.as_str());

    // If both key and value are present, this is a config set operation.
    if let (Some(key), Some(value)) = (key, value) {
        return config_set(ctx, key, value);
    }

    // Otherwise, read-only config display.
    let app_json = serde_json::to_value(&ctx.config.app).map_err(|e| {
        (
            ErrorCode::InternalError,
            format!("Failed to serialize config: {e}"),
        )
    })?;

    match key {
        None => Ok(json!({ "config": app_json })),
        Some(name) => match app_json.get(name) {
            Some(data) => Ok(json!({ "key": name, "config": data })),
            None => Err((
                ErrorCode::NotFound,
                format!("Config section not found: {name}"),
            )),
        },
    }
}

/// Set a runtime config value. Only a focused set of keys are supported.
fn config_set(ctx: &mut CommandContext, key: &str, value: &str) -> CommandResult {
    match key {
        "defaults.model" | "model" => {
            // Validate the model exists.
            let _ = ctx
                .config
                .models
                .find_model(value)
                .map_err(|e| (ErrorCode::NotFound, format!("{e}")))?;
            ctx.active_model = Some(value.to_string());
            Ok(json!({ "set": key, "value": value }))
        }
        "defaults.stream" | "stream" => {
            let v: bool = value
                .parse()
                .map_err(|_| (ErrorCode::InvalidRequest, "expected true or false".into()))?;
            ctx.config.app.defaults.stream = v;
            Ok(json!({ "set": key, "value": v }))
        }
        "autonomy.enabled" | "behavior.autonomy.enabled" => {
            let v: bool = value
                .parse()
                .map_err(|_| (ErrorCode::InvalidRequest, "expected true or false".into()))?;
            ctx.config.app.behavior.autonomy.enabled = v;
            Ok(json!({ "set": "autonomy.enabled", "value": v }))
        }
        _ => Err((
            ErrorCode::InvalidRequest,
            format!(
                "Config key not settable at runtime: {key}. Supported: defaults.model, defaults.stream, autonomy.enabled"
            ),
        )),
    }
}

/// Reset all runtime config overrides by reloading from disk.
pub fn config_reset(ctx: &mut CommandContext) -> CommandResult {
    let config_path = ctx.config_path.clone();
    match shore_config::load_config(Some(&config_path)) {
        Ok(fresh) => {
            ctx.active_model = None;
            ctx.autonomy.reload_runtime_config(fresh.clone());
            ctx.llm_client.set_usage_config(fresh.app.usage.clone());
            ctx.config = fresh;
            info!(path = %config_path.display(), "Configuration reloaded from disk");
            Ok(json!({
                "reset": true,
                "message": "Configuration reloaded from disk",
                "config_path": config_path.display().to_string(),
                "invalidated": {
                    "runtime_overrides": true,
                }
            }))
        }
        Err(e) => Err((
            ErrorCode::InternalError,
            format!("Failed to reload config: {e}"),
        )),
    }
}
