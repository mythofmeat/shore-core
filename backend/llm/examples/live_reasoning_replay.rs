//! Live OpenAI-compatible reasoning replay smoke test.
//!
//! This sends a post-tool-call continuation request through Shore's
//! OpenAI-compatible adapter. It is intentionally not part of normal tests:
//! it uses real provider credentials and may cost money.
//!
//! Usage:
//!   cargo run -p shore-llm --example live_reasoning_replay -- deepseek
//!   cargo run -p shore-llm --example live_reasoning_replay -- opencode-kimi
//!   cargo run -p shore-llm --example live_reasoning_replay -- openrouter-kimi
//!   cargo run -p shore-llm --example live_reasoning_replay -- nanogpt-deepseek
//!
//! Optional:
//!   SHORE_ENV_FILE=/path/to/.env

use std::env;
use std::fs;
use std::path::PathBuf;

use serde_json::json;
use shore_config::models::Sdk;
use shore_llm::types::LlmRequest;
use shore_llm::LlmClient;

struct Target {
    provider_key: &'static str,
    auth_provider: &'static str,
    model: &'static str,
    api_key_env: &'static str,
    base_url: &'static str,
    reasoning_effort: Option<&'static str>,
}

fn load_env_file() {
    let path = env::var("SHORE_ENV_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/home/eshen/.config/shore/.env"));
    let Ok(contents) = fs::read_to_string(path) else {
        return;
    };
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("export ") {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        env::set_var(key, value);
    }
}

fn target(name: &str) -> Option<Target> {
    match name {
        "deepseek" => Some(Target {
            provider_key: "deepseek",
            auth_provider: "deepseek",
            model: "deepseek-v4-pro",
            api_key_env: "DEEPSEEK_API_KEY",
            base_url: "https://api.deepseek.com/v1",
            reasoning_effort: Some("high"),
        }),
        "opencode-kimi" => Some(Target {
            provider_key: "opencode",
            auth_provider: "opencode-go",
            model: "kimi-k2.6",
            api_key_env: "OPENCODE_API_KEY",
            base_url: "https://opencode.ai/zen/go/v1",
            reasoning_effort: Some("high"),
        }),
        "openrouter-kimi" => Some(Target {
            provider_key: "openrouter",
            auth_provider: "openrouter",
            model: "moonshotai/kimi-k2.6",
            api_key_env: "OPENROUTER_API_KEY",
            base_url: "https://openrouter.ai/api/v1",
            reasoning_effort: Some("high"),
        }),
        "nanogpt-deepseek" => Some(Target {
            provider_key: "nanogpt",
            auth_provider: "nanogpt",
            model: "deepseek/deepseek-v4-pro:thinking",
            api_key_env: "NANOGPT_API_KEY",
            base_url: "https://nano-gpt.com/api/v1",
            reasoning_effort: Some("high"),
        }),
        _ => None,
    }
}

fn opencode_auth_key(provider: &str) -> Option<String> {
    let path = PathBuf::from("/home/eshen/.local/share/opencode/auth.json");
    let contents = fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&contents).ok()?;
    let key = value
        .get(provider)?
        .get("key")?
        .as_str()?
        .trim()
        .to_string();
    if key.is_empty() {
        None
    } else {
        Some(key)
    }
}

#[tokio::main]
async fn main() {
    load_env_file();

    let name = env::args().nth(1).unwrap_or_else(|| {
        eprintln!(
            "usage: live_reasoning_replay <deepseek|opencode-kimi|openrouter-kimi|nanogpt-deepseek>"
        );
        std::process::exit(2);
    });
    let Some(target) = target(&name) else {
        eprintln!("unknown target: {name}");
        std::process::exit(2);
    };

    let api_key = env::var(target.api_key_env)
        .ok()
        .or_else(|| opencode_auth_key(target.auth_provider))
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| {
            eprintln!(
                "{} is not set and opencode auth has no {} key",
                target.api_key_env, target.auth_provider
            );
            std::process::exit(2);
        });

    let provider_options = target.reasoning_effort.map(|effort| {
        json!({
            "reasoning_effort": effort,
        })
    });

    let request = LlmRequest {
        sdk: Sdk::Openai,
        model: target.model.to_string(),
        api_key,
        base_url: Some(target.base_url.to_string()),
        messages: vec![
            json!({
                "role": "user",
                "content": "Use the tool result and answer in one short sentence."
            }),
            json!({
                "role": "assistant",
                "content": [
                    {
                        "type": "thinking",
                        "thinking": "I need the lookup result before answering."
                    },
                    {
                        "type": "tool_use",
                        "id": "call_live_reasoning_1",
                        "name": "lookup_fact",
                        "input": {"topic": "live smoke test"}
                    }
                ]
            }),
            json!({
                "role": "user",
                "content": [
                    {
                        "type": "tool_result",
                        "tool_use_id": "call_live_reasoning_1",
                        "content": "The lookup result is: live reasoning replay succeeded."
                    }
                ]
            }),
        ],
        system: Some(json!("You are a concise live API smoke-test assistant.")),
        tools: Some(vec![json!({
            "name": "lookup_fact",
            "description": "Looks up one short fact for a live smoke test.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "topic": {"type": "string"}
                },
                "required": ["topic"]
            }
        })]),
        max_tokens: 96,
        temperature: Some(0.0),
        top_p: None,
        provider_options,
        provider_key: Some(target.provider_key.to_string()),
        rid: Some(format!("live-reasoning-replay-{name}")),
        forensic_character: None,
        system_suffix: None,
        retain_long: false,
    };

    let client = LlmClient::new();
    match client.generate(&request).await {
        Ok(resp) => {
            let text = resp.extract_text();
            println!(
                "PASS target={name} model={} finish_reason={} input_tokens={} output_tokens={} text={}",
                resp.model,
                resp.finish_reason,
                resp.usage.input_tokens,
                resp.usage.output_tokens,
                text.trim().replace('\n', " ")
            );
        }
        Err(err) => {
            eprintln!("FAIL target={name} error={err}");
            std::process::exit(1);
        }
    }
}
