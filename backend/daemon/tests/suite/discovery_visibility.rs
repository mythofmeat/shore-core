//! Phase 10 — Discovery + visibility setup E2E.
//!
//! Boots a daemon with `[providers.openrouter]` discovery enabled and a
//! gitignore-style visibility filter, then pre-seeds the on-disk
//! `<data>/providers/openrouter/models.json` cache with three models.
//! No real `/v1/models` fetch happens — the cache is the source of
//! truth for this test.
//!
//! Asserts:
//!
//! * `list_provider_models` splits cache rows into `discovered` (visible)
//!   and `hidden` according to the visibility patterns (last match wins).
//! * `include_hidden = true` collapses the split into one full list.
//! * The effective catalog returned by `list_models` exposes only the
//!   visible discovered rows by default and tags them `source = "discovered"`.
//! * `list_models` with `include_hidden = true` reveals every cache row.
//! * Static aliases are never filtered, even when the visibility rules
//!   would otherwise hide their `(provider, model_id)` pair.

use serde_json::{json, Value};
use shore_llm::discovery::{
    cache_path, write_cache, DiscoveredModel, ProviderModelsCache, CACHE_VERSION,
};
use shore_protocol::server_msg::{CommandOutput, ServerMessage};
use shore_test_harness::{TestConfigBuilder, TestHarness};

fn extract(messages: &[ServerMessage], expected_cmd: &str) -> Value {
    messages
        .iter()
        .find_map(|m| match m {
            ServerMessage::CommandOutput(CommandOutput { name, data, .. })
                if name == expected_cmd =>
            {
                Some(data.clone())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("no CommandOutput for {expected_cmd}: {messages:#?}"))
}

fn fixture(provider: &str, model_id: &str) -> DiscoveredModel {
    DiscoveredModel {
        provider_key: provider.into(),
        model_id: model_id.into(),
        display_name: None,
        sdk: "openai".into(),
        base_url: Some("https://example.test/v1".into()),
        created_at: None,
        owned_by: None,
        description: None,
        context_length: None,
        max_output_tokens: None,
        supports_tools: None,
        supports_images: None,
        supports_reasoning: None,
        supports_prompt_cache: None,
        raw_provider_metadata: Value::Null,
        discovered_at: "2026-04-29T00:00:00Z".into(),
    }
}

fn seed_cache(data_dir: &std::path::Path, provider: &str, ids: &[&str]) {
    let cache = ProviderModelsCache {
        version: CACHE_VERSION,
        provider_key: provider.into(),
        fetched_at: "2026-04-29T00:00:00Z".into(),
        base_url: Some("https://example.test/v1".into()),
        models: ids.iter().map(|id| fixture(provider, id)).collect(),
    };
    write_cache(&cache_path(data_dir, provider), &cache).expect("write_cache");
}

fn ids(arr: &Value) -> Vec<String> {
    arr.as_array()
        .unwrap_or_else(|| panic!("expected array, got {arr:?}"))
        .iter()
        .map(|m| m["model_id"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn discovery_and_visibility_filter_listings_end_to_end() {
    // Visibility: hide everything by default, then re-expose anthropic/*.
    // Last match wins in gitignore-style rules.
    let registry = r#"
[providers.openrouter]
sdk = "anthropic"
api_key_env = "SHORE_TEST_API_KEY"

[providers.openrouter.discovery]
enabled = true
visibility = ["*", "!anthropic/*"]
"#;

    let harness =
        TestHarness::boot_with(TestConfigBuilder::new().provider_registry_toml(registry)).await;

    seed_cache(
        &harness.data_dir,
        "openrouter",
        &[
            "anthropic/claude-sonnet-4.5",
            "openai/gpt-4o",
            "meta-llama/llama-3-405b",
        ],
    );

    let mut harness = harness;

    // ── list_provider_models without --all ──────────────────────────────
    let messages = harness
        .send_command_with_args("list_provider_models", json!({ "provider": "openrouter" }))
        .await;
    let data = extract(&messages, "list_provider_models");
    let visible = ids(&data["discovered"]);
    let hidden = ids(&data["hidden"]);
    assert_eq!(visible, vec!["anthropic/claude-sonnet-4.5"]);
    assert_eq!(
        hidden,
        vec!["openai/gpt-4o", "meta-llama/llama-3-405b"],
        "hidden list must contain everything outside anthropic/*"
    );
    assert_eq!(data["include_hidden"], false);

    // ── list_provider_models with include_hidden ────────────────────────
    let messages = harness
        .send_command_with_args(
            "list_provider_models",
            json!({ "provider": "openrouter", "include_hidden": true }),
        )
        .await;
    let data = extract(&messages, "list_provider_models");
    assert_eq!(data["discovered"].as_array().unwrap().len(), 3);
    assert!(data["hidden"].as_array().unwrap().is_empty());
    assert_eq!(data["include_hidden"], true);

    // ── list_models (effective catalog) without --all ────────────────────
    let messages = harness.send_command("list_models").await;
    let data = extract(&messages, "list_models");
    let entries = data["models"].as_array().unwrap();
    let discovered: Vec<&str> = entries
        .iter()
        .filter(|m| m["source"] == "discovered")
        .map(|m| m["model_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        discovered,
        vec!["anthropic/claude-sonnet-4.5"],
        "effective catalog must hide non-anthropic discovered rows by default"
    );
    // Static haiku alias remains visible regardless of visibility rules.
    assert!(entries
        .iter()
        .any(|m| m["name"] == "haiku" && m["source"] == "static"));
    assert_eq!(data["hidden_count"], 2);

    // ── list_models with include_hidden ─────────────────────────────────
    let messages = harness
        .send_command_with_args("list_models", json!({ "include_hidden": true }))
        .await;
    let data = extract(&messages, "list_models");
    let entries = data["models"].as_array().unwrap();
    let discovered_ids: Vec<&str> = entries
        .iter()
        .filter(|m| m["source"] == "discovered")
        .map(|m| m["model_id"].as_str().unwrap())
        .collect();
    assert_eq!(discovered_ids.len(), 3);
    assert!(discovered_ids.contains(&"openai/gpt-4o"));
    assert!(discovered_ids.contains(&"meta-llama/llama-3-405b"));

    harness.shutdown().await;
}
