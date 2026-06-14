use std::fmt::Write as _;
use std::path::Path;

use shore_config::{
    app::{AppConfig, CompactionConfig, HeartbeatConfig, ResponseDelayConfig, ToolsConfig},
    duration::ConfigDuration,
    models::ModelCatalog,
    providers::ProviderRegistry,
    LoadedConfig, ShoreDirs,
};

type BuildResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// `provider:model_id` identity for the harness embedding model. Transport
/// (`base_url` = mock, `api_key_env` = `SHORE_TEST_API_KEY`) is injected as a
/// `[providers.testembed]` entry in `apply_provider_registry`.
const TEST_EMBED_REF: &str = "testembed:text-embedding-3-small";

#[must_use]
#[expect(
    clippy::struct_excessive_bools,
    reason = "test harness builder mirrors daemon config toggles for concise tests"
)]
#[derive(Debug, Clone)]
pub struct TestConfigBuilder {
    pub character_name: String,
    pub character_definition: String,
    pub model_alias: String,
    pub model_id: String,
    pub max_output_tokens: u32,
    pub cache_ttl: Option<String>,
    pub tool_use_enabled: bool,
    pub compaction_enabled: bool,
    pub compaction_max_turns: Option<usize>,
    pub compaction_min_turns: Option<usize>,
    pub compaction_keep_recent: Option<usize>,
    pub autonomy_enabled: bool,
    /// The unified per-model tool-iteration cap. `None` = unlimited (the
    /// production default). When set, the harness writes it as a global
    /// per-model preference for the chat model so every tool loop (chat,
    /// heartbeat, compaction, dreaming) reads the same cap.
    pub max_tool_iterations: Option<u32>,
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
    /// Optional `[behavior.response_delay]` config. `None` leaves it disabled
    /// (the production default).
    pub response_delay: Option<ResponseDelayConfig>,
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
            max_output_tokens: 1024,
            cache_ttl: None,
            tool_use_enabled: true,
            compaction_enabled: false,
            compaction_max_turns: None,
            compaction_min_turns: None,
            compaction_keep_recent: None,
            autonomy_enabled: false,
            max_tool_iterations: None,
            api_payload_logging: false,
            provider_registry_toml: None,
            extra_chat_aliases: Vec::new(),
            extra_characters: Vec::new(),
            response_delay: None,
        }
    }

    /// Enable `[behavior.response_delay]` with the given config.
    pub fn response_delay(mut self, cfg: ResponseDelayConfig) -> Self {
        self.response_delay = Some(cfg);
        self
    }

    /// Inject a `[providers.<name>]` registry section. The string must be
    /// a complete TOML fragment (without a wrapping `[providers]` table —
    /// the parser expects each entry as `[providers.<name>]`).
    pub fn provider_registry_toml(mut self, toml: &str) -> Self {
        self.provider_registry_toml = Some(toml.to_owned());
        self
    }

    /// Add a second chat alias under the same `[openrouter]` provider.
    /// `alias` is the short name (e.g. `"sonnet"`), `model_id` is the
    /// upstream id. Phase 10 persistence tests use this to switch
    /// between two real catalog entries on a single boot.
    pub fn extra_chat_alias(mut self, alias: &str, model_id: &str) -> Self {
        self.extra_chat_aliases
            .push((alias.to_owned(), model_id.to_owned()));
        self
    }

    /// Pre-create an additional character workspace alongside the primary
    /// one. Used by Phase 10 per-character preference tests.
    pub fn extra_character(mut self, name: &str, definition: &str) -> Self {
        self.extra_characters
            .push((name.to_owned(), definition.to_owned()));
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

    /// Set the unified per-model tool-iteration cap that governs every tool
    /// loop (chat, heartbeat, compaction, dreaming). Written as a per-character
    /// preference for the configured chat model.
    ///
    /// Panics on `0` to mirror the production rejection in
    /// `set_model_setting` — `0` is not a persistable value (unlimited is
    /// expressed by leaving the cap unset), so tests must not fabricate it.
    pub fn max_tool_iterations(mut self, n: u32) -> Self {
        assert!(
            n >= 1,
            "max_tool_iterations must be >= 1; leave it unset for unlimited"
        );
        self.max_tool_iterations = Some(n);
        self
    }

    /// Cap the number of tool-use rounds per heartbeat tick. A cap of 1 with
    /// a queued tool_use response at iteration 0 forces `hit_cap` → wrap-up.
    ///
    /// Alias for [`Self::max_tool_iterations`]: the cap is now a single
    /// per-model surface, so the heartbeat shares it with the other loops.
    pub fn heartbeat_max_tool_rounds(self, n: u32) -> Self {
        self.max_tool_iterations(n)
    }

    /// Enable per-call API payload logging on the harness's `LlmClient`,
    /// routing files under `<cache>/debug/api_logs/` (chat) and
    /// `<cache>/debug/api_logs_long/` (background, when the call sets
    /// `LlmRequest::retain_long`).
    pub fn api_payload_logging(mut self, enabled: bool) -> Self {
        self.api_payload_logging = enabled;
        self
    }

    #[expect(
        clippy::panic,
        reason = "legacy test harness convenience API fails fast during setup"
    )]
    pub fn build(&self, tmp_dir: &Path, mock_base_url: &str) -> LoadedConfig {
        match self.try_build(tmp_dir, mock_base_url) {
            Ok(config) => config,
            Err(error) => panic!("failed to build test config: {error}"),
        }
    }

    pub fn try_build(&self, tmp_dir: &Path, mock_base_url: &str) -> BuildResult<LoadedConfig> {
        // Set a dummy API key so LlmClient::build_request succeeds.
        std::env::set_var("SHORE_TEST_API_KEY", "sk-test-dummy");

        let config_dir = tmp_dir.join("config");
        let data_dir = tmp_dir.join("data");
        let runtime_dir = tmp_dir.join("runtime");

        std::fs::create_dir_all(&config_dir)?;
        std::fs::create_dir_all(&data_dir)?;
        std::fs::create_dir_all(&runtime_dir)?;
        self.write_character_files(&config_dir)?;

        let models = self.build_model_catalog(mock_base_url)?;
        let mut loaded = LoadedConfig::new_for_test(
            self.build_app_config(),
            models,
            ShoreDirs {
                config: config_dir,
                data: data_dir,
                runtime: runtime_dir,
                cache: tmp_dir.join("cache"),
            },
        );
        self.apply_provider_registry(&mut loaded, mock_base_url)?;

        if let Some(n) = self.max_tool_iterations {
            write_max_tool_iterations_pref(&loaded, &self.character_name, &self.model_alias, n)?;
        }

        Ok(loaded)
    }

    fn write_character_files(&self, config_dir: &Path) -> std::io::Result<()> {
        let characters_dir = config_dir.join("characters");
        write_character_file(
            &characters_dir,
            &self.character_name,
            &self.character_definition,
        )?;
        for (extra_name, extra_def) in &self.extra_characters {
            write_character_file(&characters_dir, extra_name, extra_def)?;
        }
        Ok(())
    }

    fn build_app_config(&self) -> AppConfig {
        let mut app = AppConfig::default();
        app.defaults.model = Some(self.model_alias.clone());
        // Tools are opt-in: when enabled, allowlist the whole registered set so
        // tests have the full surface; otherwise leave the allowlist empty.
        app.tools = if self.tool_use_enabled {
            ToolsConfig {
                enabled_tools: shore_daemon::tools::all_tools()
                    .iter()
                    .map(|t| t.name.to_owned())
                    .collect(),
                ..ToolsConfig::default()
            }
        } else {
            ToolsConfig::default()
        };
        app.behavior.autonomy.enabled = self.autonomy_enabled;
        if let Some(response_delay) = &self.response_delay {
            app.behavior.response_delay = response_delay.clone();
        }
        app.advanced.api_payload_logging = self.api_payload_logging;

        if self.max_tool_iterations.is_some() {
            app.behavior.autonomy.heartbeat = HeartbeatConfig {
                // Long intervals so spontaneous ticks don't fire during the
                // test. The caller drives manual ticks and virtual time.
                fallback_heartbeat_interval: ConfigDuration::from_secs(86400),
                minimum_heartbeat_latency: ConfigDuration::from_secs(86400),
                ..HeartbeatConfig::default()
            };
        }

        if self.compaction_enabled {
            app.memory.compaction = CompactionConfig {
                enabled: true,
                idle_trigger: ConfigDuration::from_secs(86400),
                archive_after: ConfigDuration::from_secs(0),
                min_turns: self.compaction_min_turns.unwrap_or(2),
                max_turns: self.compaction_max_turns.unwrap_or(16),
                max_context_tokens: 0,
                keep_recent_turns: self.compaction_keep_recent.unwrap_or(2),
            };
            app.defaults.embedding = Some(TEST_EMBED_REF.into());
        }

        app
    }

    fn build_model_catalog(&self, mock_base_url: &str) -> BuildResult<ModelCatalog> {
        let chat_table: toml::Table = self.models_toml(mock_base_url).parse()?;
        let embed_table = self.embed_toml().map(|toml| toml.parse()).transpose()?;
        Ok(ModelCatalog::from_sections(
            Some(&chat_table),
            embed_table.as_ref(),
            None,
        )?)
    }

    fn models_toml(&self, mock_base_url: &str) -> String {
        let model_alias = &self.model_alias;
        let model_id = &self.model_id;
        let max_output_tokens = self.max_output_tokens;
        // Transport (base_url/sdk/api_key_env) lives on each model sub-table —
        // provider-level scalars under `[chat.<provider>]` were retired (#137).
        let mut models_toml = format!(
            r#"
[openrouter.{model_alias}]
model_id = "{model_id}"
base_url = "{mock_base_url}"
sdk = "anthropic"
api_key_env = "SHORE_TEST_API_KEY"
max_output_tokens = {max_output_tokens}
temperature = 0.0
"#,
        );
        push_cache_ttl(&mut models_toml, self.cache_ttl.as_deref());
        for (extra_alias, extra_model_id) in &self.extra_chat_aliases {
            let _ignored = write!(
                models_toml,
                "\n[openrouter.{extra_alias}]\nmodel_id = \"{extra_model_id}\"\nbase_url = \"{mock_base_url}\"\nsdk = \"anthropic\"\napi_key_env = \"SHORE_TEST_API_KEY\"\nmax_output_tokens = {max_output_tokens}\ntemperature = 0.0\n",
            );
            push_cache_ttl(&mut models_toml, self.cache_ttl.as_deref());
        }
        models_toml
    }

    fn embed_toml(&self) -> Option<String> {
        // New shape: identity is the `provider:model_id` key; transport lives on
        // the `[providers.testembed]` entry injected in `apply_provider_registry`.
        self.compaction_enabled.then(|| {
            format!(
                r#"
["{TEST_EMBED_REF}"]
dimensions = 8
"#,
            )
        })
    }

    fn apply_provider_registry(
        &self,
        loaded: &mut LoadedConfig,
        mock_base_url: &str,
    ) -> BuildResult<()> {
        // Start from any caller-supplied `[providers]` section, then inject the
        // embedding provider when compaction is enabled. The embedder resolves
        // transport + credentials through this registry (mirroring chat), so the
        // `testembed` provider must carry the mock base_url and the dummy key env.
        let mut providers_section: toml::Table = match self.provider_registry_toml {
            Some(ref toml_text) => {
                let table: toml::Table = toml_text.parse()?;
                table
                    .get("providers")
                    .and_then(|v| v.as_table())
                    .cloned()
                    .unwrap_or_default()
            }
            None => toml::Table::new(),
        };

        if self.compaction_enabled && !providers_section.contains_key("testembed") {
            let entry: toml::Value =
                format!("base_url = \"{mock_base_url}\"\napi_key_env = \"SHORE_TEST_API_KEY\"\n")
                    .parse::<toml::Table>()?
                    .into();
            let _ignored = providers_section.insert("testembed".to_owned(), entry);
        }

        if !providers_section.is_empty() {
            loaded.providers = ProviderRegistry::from_section(Some(&providers_section))?;
        }
        Ok(())
    }
}

fn write_character_file(
    characters_dir: &Path,
    name: &str,
    definition: &str,
) -> std::io::Result<()> {
    let char_dir = characters_dir.join(name);
    std::fs::create_dir_all(&char_dir)?;
    std::fs::write(char_dir.join("character.md"), definition)
}

fn push_cache_ttl(models_toml: &mut String, cache_ttl: Option<&str>) {
    if let Some(ttl) = cache_ttl {
        let _ignored = writeln!(models_toml, "cache_ttl = \"{ttl}\"");
    }
}

/// Persist the unified per-model `max_tool_iterations` cap as a character
/// preference keyed on the resolved chat model, so every tool loop (chat,
/// heartbeat, compaction, dreaming) reads it through `resolve_*_model`.
/// Character-scoped (not global) so the file lives under the character root and
/// does not create a sibling directory under `data_dir`.
///
/// Keys on the model `model_alias` resolves to (which `build_app_config` pins as
/// `app.defaults.model`), not `first_chat_model()` — with `extra_chat_aliases`
/// those can diverge, and the cap must land on the model the loops actually run.
fn write_max_tool_iterations_pref(
    loaded: &LoadedConfig,
    character: &str,
    model_alias: &str,
    n: u32,
) -> BuildResult<()> {
    use shore_daemon::preferences::{
        save_character_preferences, ModelPreference, ModelPreferences, SamplerSettings,
    };
    let model = loaded.models.find_model(model_alias)?;
    let key = format!("{}:{}", model.provider_key, model.model_id);
    let mut prefs = ModelPreferences::default();
    let _ignored = prefs.models.insert(
        key,
        ModelPreference {
            sampler: SamplerSettings {
                max_tool_iterations: Some(n),
                ..Default::default()
            },
        },
    );
    save_character_preferences(&loaded.dirs.data, character, &prefs)?;
    Ok(())
}
