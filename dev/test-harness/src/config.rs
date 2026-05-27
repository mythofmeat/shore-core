use std::path::Path;

use shore_config::{
    app::{AppConfig, BehaviorConfig, CompactionConfig, HeartbeatConfig, ToolUseConfig},
    duration::ConfigDuration,
    models::ModelCatalog,
    providers::ProviderRegistry,
    LoadedConfig, ShoreDirs,
};

#[derive(Clone)]
pub struct TestConfigBuilder {
    pub character_name: String,
    pub character_definition: String,
    pub model_alias: String,
    pub model_id: String,
    pub max_tokens: u32,
    pub cache_ttl: Option<String>,
    pub tool_use_enabled: bool,
    pub tool_use_max_iterations: u32,
    pub compaction_enabled: bool,
    pub compaction_max_turns: Option<usize>,
    pub compaction_min_turns: Option<usize>,
    pub compaction_keep_recent: Option<usize>,
    pub autonomy_enabled: bool,
    pub heartbeat_max_tool_rounds: Option<u32>,
    /// If true, the harness enables `[advanced].api_payload_logging` and
    /// wires `LlmClient::set_payload_log_dir` to the cache dir. Tests that
    /// inspect `<cache>/debug/api_logs{,_long}/` need this flipped on.
    pub api_payload_logging: bool,
    /// Optional `[providers.<name>]` registry section in TOML form.
    /// When set, parsed and attached to `LoadedConfig.providers`. Used by
    /// the multi-key fallback tests to declare ordered named keys.
    pub provider_registry_toml: Option<String>,
    /// Additional `[openrouter.<alias>]` chat aliases to add alongside the
    /// primary `model_alias`. Each entry is `(alias, model_id)`. Used by
    /// per-model preference persistence tests that need two switchable
    /// catalog entries on a single provider.
    pub extra_chat_aliases: Vec<(String, String)>,
    /// Extra characters whose `character.md` files should be written into
    /// the config dir before boot. Used by per-character preference tests
    /// that need to switch the active character without restarting.
    pub extra_characters: Vec<(String, String)>,
}

impl Default for TestConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl TestConfigBuilder {
    pub fn new() -> Self {
        Self {
            character_name: "TestChar".into(),
            character_definition:
                "You are TestChar, a concise test assistant. Keep responses very short (1-2 sentences)."
                    .into(),
            model_alias: "haiku".into(),
            model_id: "anthropic/claude-haiku-4.5".into(),
            max_tokens: 1024,
            cache_ttl: None,
            tool_use_enabled: true,
            tool_use_max_iterations: 5,
            compaction_enabled: false,
            compaction_max_turns: None,
            compaction_min_turns: None,
            compaction_keep_recent: None,
            autonomy_enabled: false,
            heartbeat_max_tool_rounds: None,
            api_payload_logging: false,
            provider_registry_toml: None,
            extra_chat_aliases: Vec::new(),
            extra_characters: Vec::new(),
        }
    }

    /// Inject a `[providers.<name>]` registry section. The string must be
    /// a complete TOML fragment (without a wrapping `[providers]` table —
    /// the parser expects each entry as `[providers.<name>]`).
    pub fn provider_registry_toml(mut self, toml: &str) -> Self {
        self.provider_registry_toml = Some(toml.to_string());
        self
    }

    /// Add a second chat alias under the same `[openrouter]` provider.
    /// `alias` is the short name (e.g. `"sonnet"`), `model_id` is the
    /// upstream id. Phase 10 persistence tests use this to switch
    /// between two real catalog entries on a single boot.
    pub fn extra_chat_alias(mut self, alias: &str, model_id: &str) -> Self {
        self.extra_chat_aliases
            .push((alias.to_string(), model_id.to_string()));
        self
    }

    /// Pre-create an additional character workspace alongside the primary
    /// one. Used by Phase 10 per-character preference tests.
    pub fn extra_character(mut self, name: &str, definition: &str) -> Self {
        self.extra_characters
            .push((name.to_string(), definition.to_string()));
        self
    }

    pub fn character_name(mut self, name: &str) -> Self {
        self.character_name = name.into();
        self
    }

    pub fn character_definition(mut self, def: &str) -> Self {
        self.character_definition = def.into();
        self
    }

    pub fn tool_use(mut self, enabled: bool) -> Self {
        self.tool_use_enabled = enabled;
        self
    }

    pub fn cache_ttl(mut self, ttl: &str) -> Self {
        self.cache_ttl = Some(ttl.into());
        self
    }

    pub fn compaction(mut self, enabled: bool) -> Self {
        self.compaction_enabled = enabled;
        self
    }

    pub fn compaction_max_turns(mut self, n: usize) -> Self {
        self.compaction_max_turns = Some(n);
        self
    }

    pub fn compaction_min_turns(mut self, n: usize) -> Self {
        self.compaction_min_turns = Some(n);
        self
    }

    pub fn compaction_keep_recent(mut self, n: usize) -> Self {
        self.compaction_keep_recent = Some(n);
        self
    }

    pub fn autonomy(mut self, enabled: bool) -> Self {
        self.autonomy_enabled = enabled;
        self
    }

    /// Cap the number of tool-use rounds per heartbeat tick. A cap of 1 with
    /// a queued tool_use response at iteration 0 forces `hit_cap` → wrap-up.
    pub fn heartbeat_max_tool_rounds(mut self, n: u32) -> Self {
        self.heartbeat_max_tool_rounds = Some(n);
        self
    }

    /// Enable per-call API payload logging on the harness's `LlmClient`,
    /// routing files under `<cache>/debug/api_logs/` (chat) and
    /// `<cache>/debug/api_logs_long/` (background, when the call sets
    /// `LlmRequest::retain_long`).
    pub fn api_payload_logging(mut self, enabled: bool) -> Self {
        self.api_payload_logging = enabled;
        self
    }

    pub fn build(&self, tmp_dir: &Path, mock_base_url: &str) -> LoadedConfig {
        // Set a dummy API key so LlmClient::build_request succeeds.
        std::env::set_var("SHORE_TEST_API_KEY", "sk-test-dummy");

        let config_dir = tmp_dir.join("config");
        let data_dir = tmp_dir.join("data");
        let runtime_dir = tmp_dir.join("runtime");

        // Create required directories.
        std::fs::create_dir_all(&config_dir).expect("failed to create config dir");
        std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
        std::fs::create_dir_all(&runtime_dir).expect("failed to create runtime dir");

        // Write character definition file.
        let char_dir = config_dir.join("characters").join(&self.character_name);
        std::fs::create_dir_all(&char_dir).expect("failed to create character dir");
        std::fs::write(char_dir.join("character.md"), &self.character_definition)
            .expect("failed to write character.md");

        // Pre-create any additional character workspaces requested by the
        // builder. They share the same model defaults — only the
        // identifier and prompt change.
        for (extra_name, extra_def) in &self.extra_characters {
            let extra_dir = config_dir.join("characters").join(extra_name);
            std::fs::create_dir_all(&extra_dir).expect("failed to create extra character dir");
            std::fs::write(extra_dir.join("character.md"), extra_def)
                .expect("failed to write extra character.md");
        }

        // Build AppConfig.
        let mut app = AppConfig::default();
        app.defaults.model = Some(self.model_alias.clone());
        app.behavior = BehaviorConfig {
            tool_use: ToolUseConfig {
                enabled: self.tool_use_enabled,
                max_iterations: self.tool_use_max_iterations,
                ..ToolUseConfig::default()
            },
            ..BehaviorConfig::default()
        };
        app.behavior.autonomy.enabled = self.autonomy_enabled;
        app.advanced.api_payload_logging = self.api_payload_logging;
        if let Some(rounds) = self.heartbeat_max_tool_rounds {
            app.behavior.autonomy.heartbeat = HeartbeatConfig {
                max_tool_rounds: rounds,
                // Long intervals so spontaneous ticks don't fire during the
                // test — the caller drives the tick manually with
                // `AutonomyManager::heartbeat_tick_now` and advances virtual
                // time to fire the per-character tick loop.
                fallback_heartbeat_interval: ConfigDuration::from_secs(86400),
                minimum_heartbeat_latency: ConfigDuration::from_secs(86400),
                ..HeartbeatConfig::default()
            };
        }

        if self.compaction_enabled {
            app.memory.compaction = CompactionConfig {
                enabled: true,
                idle_trigger: ConfigDuration::from_secs(86400), // very long — tests use max_turns
                min_turns: self.compaction_min_turns.unwrap_or(2),
                max_turns: self.compaction_max_turns.unwrap_or(16),
                max_context_tokens: 0,
                keep_recent_turns: self.compaction_keep_recent.unwrap_or(2),
                ..CompactionConfig::default()
            };
            // Also set a default embedding profile name.
            app.defaults.embedding = Some("test-embed".into());
        }
        // Build ModelCatalog from TOML pointing at the mock server.
        let mut models_toml = format!(
            r#"
[openrouter]
base_url = "{base_url}"
sdk = "anthropic"
api_key_env = "SHORE_TEST_API_KEY"

[openrouter.{alias}]
model_id = "{model_id}"
max_tokens = {max_tokens}
temperature = 0.0
"#,
            base_url = mock_base_url,
            alias = self.model_alias,
            model_id = self.model_id,
            max_tokens = self.max_tokens,
        );
        if let Some(cache_ttl) = &self.cache_ttl {
            models_toml.push_str(&format!("cache_ttl = \"{cache_ttl}\"\n"));
        }
        for (extra_alias, extra_model_id) in &self.extra_chat_aliases {
            models_toml.push_str(&format!(
                "\n[openrouter.{alias}]\nmodel_id = \"{model_id}\"\nmax_tokens = {max_tokens}\ntemperature = 0.0\n",
                alias = extra_alias,
                model_id = extra_model_id,
                max_tokens = self.max_tokens,
            ));
            if let Some(cache_ttl) = &self.cache_ttl {
                models_toml.push_str(&format!("cache_ttl = \"{cache_ttl}\"\n"));
            }
        }
        let chat_table: toml::Table = models_toml.parse().expect("failed to parse model TOML");

        let embed_table: Option<toml::Table> = if self.compaction_enabled {
            let embed_toml = format!(
                r#"
[test-embed]
model_id = "text-embedding-3-small"
provider = "openai"
api_key_env = "SHORE_TEST_API_KEY"
base_url = "{base_url}"
dimensions = 8
"#,
                base_url = mock_base_url,
            );
            Some(embed_toml.parse().expect("failed to parse embed TOML"))
        } else {
            None
        };

        let models =
            ModelCatalog::from_sections(Some(&chat_table), None, embed_table.as_ref(), None)
                .expect("failed to build ModelCatalog");

        let mut loaded = LoadedConfig::new_for_test(
            app,
            models,
            ShoreDirs {
                config: config_dir,
                data: data_dir,
                runtime: runtime_dir,
                cache: tmp_dir.join("cache"),
            },
        );

        if let Some(ref toml_text) = self.provider_registry_toml {
            let table: toml::Table = toml_text
                .parse()
                .expect("failed to parse provider_registry_toml");
            let providers_section = table.get("providers").and_then(|v| v.as_table());
            loaded.providers = ProviderRegistry::from_section(providers_section)
                .expect("failed to build ProviderRegistry from test toml");
        }

        loaded
    }
}
