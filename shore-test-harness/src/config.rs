use std::path::Path;

use shore_config::{
    LoadedConfig, ShoreDirs,
    app::{AppConfig, BehaviorConfig, CompactionConfig, ToolUseConfig},
    duration::ConfigDuration,
    models::ModelCatalog,
};

pub struct TestConfigBuilder {
    pub character_name: String,
    pub character_definition: String,
    pub model_alias: String,
    pub model_id: String,
    pub max_tokens: u32,
    pub tool_use_enabled: bool,
    pub tool_use_max_iterations: u32,
    pub compaction_enabled: bool,
    pub compaction_max_turns: Option<usize>,
    pub compaction_min_turns: Option<usize>,
    pub compaction_keep_recent: Option<usize>,
    pub autonomy_enabled: bool,
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
            tool_use_enabled: true,
            tool_use_max_iterations: 5,
            compaction_enabled: false,
            compaction_max_turns: None,
            compaction_min_turns: None,
            compaction_keep_recent: None,
            autonomy_enabled: false,
        }
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
        let char_dir = config_dir
            .join("characters")
            .join(&self.character_name);
        std::fs::create_dir_all(&char_dir).expect("failed to create character dir");
        std::fs::write(char_dir.join("character.md"), &self.character_definition)
            .expect("failed to write character.md");

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

        if self.compaction_enabled {
            app.memory.compaction = CompactionConfig {
                enabled: true,
                idle_trigger: ConfigDuration::from_secs(86400), // very long — tests use max_turns
                min_turns: self.compaction_min_turns.unwrap_or(2),
                max_turns: self.compaction_max_turns.unwrap_or(16),
                keep_recent_turns: self.compaction_keep_recent.unwrap_or(2),
            };
            // Also set a default embedding profile name.
            app.defaults.embedding = Some("test-embed".into());
        }
        // Disable collation auto_run to prevent post-compaction collation.
        app.memory.collation.auto_run = false;

        // Build ModelCatalog from TOML pointing at the mock server.
        let models_toml = format!(
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

        let models = ModelCatalog::from_sections(
            Some(&chat_table),
            None,
            embed_table.as_ref(),
            None,
        )
        .expect("failed to build ModelCatalog");

        LoadedConfig::new_for_test(
            app,
            models,
            ShoreDirs {
                config: config_dir,
                data: data_dir,
                runtime: runtime_dir,
            },
        )
    }
}
