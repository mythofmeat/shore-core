pub mod app;
pub mod duration;
pub mod models;
pub mod providers;

pub use duration::ConfigDuration;

use std::path::{Path, PathBuf};

use app::AppConfig;
use models::ModelCatalog;
use providers::ProviderRegistry;
use tracing::{info, warn};

/// Errors that can occur during configuration loading.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse config.toml: {0}")]
    ParseApp(#[source] toml::de::Error),

    #[error("failed to parse include file {path}: {source}")]
    ParseInclude {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("failed to parse conf.d file {path}: {source}")]
    ConfD {
        path: PathBuf,
        source: toml::de::Error,
    },

    #[error("failed to parse model catalog: {0}")]
    Catalog(Box<models::CatalogError>),

    #[error("failed to parse provider registry: {0}")]
    ProviderRegistry(#[source] providers::ProviderRegistryError),

    #[error("validation error: {0}")]
    Validation(String),
}

impl From<models::CatalogError> for ConfigError {
    fn from(e: models::CatalogError) -> Self {
        ConfigError::Catalog(Box::new(e))
    }
}

impl From<providers::ProviderRegistryError> for ConfigError {
    fn from(e: providers::ProviderRegistryError) -> Self {
        ConfigError::ProviderRegistry(e)
    }
}

/// Resolved XDG directory paths for Shore.
#[derive(Debug, Clone)]
pub struct ShoreDirs {
    /// Config directory: $XDG_CONFIG_HOME/shore/
    pub config: PathBuf,
    /// Data directory: $XDG_DATA_HOME/shore/
    pub data: PathBuf,
    /// Runtime directory: $XDG_RUNTIME_DIR/shore/
    pub runtime: PathBuf,
    /// Cache directory: $XDG_CACHE_HOME/shore/
    pub cache: PathBuf,
}

/// Resolve an XDG-style directory path with Shore-specific overrides.
///
/// Precedence: `override_var` → `xdg_var`+"/shore" → `platform_fn()`+"/shore" → `fallback`+"/shore".
/// If `fallback` is empty, `std::env::temp_dir()` is used.
fn resolve_xdg_dir(
    override_var: &str,
    xdg_var: &str,
    platform_fn: fn() -> Option<PathBuf>,
    fallback: &str,
) -> PathBuf {
    std::env::var(override_var)
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var(xdg_var)
                .ok()
                .map(PathBuf::from)
                .or_else(platform_fn)
                .unwrap_or_else(|| {
                    if fallback.is_empty() {
                        std::env::temp_dir()
                    } else {
                        PathBuf::from(fallback)
                    }
                })
                .join("shore")
        })
}

impl ShoreDirs {
    /// Resolve Shore directories.
    ///
    /// Priority (highest first):
    /// 1. `SHORE_CONFIG_DIR` / `SHORE_DATA_DIR` / `SHORE_RUNTIME_DIR` — used as-is
    /// 2. `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_RUNTIME_DIR` + `/shore`
    /// 3. Platform defaults + `/shore`
    pub fn resolve() -> Self {
        Self {
            config: resolve_xdg_dir(
                "SHORE_CONFIG_DIR",
                "XDG_CONFIG_HOME",
                dirs::config_dir,
                "~/.config",
            ),
            data: resolve_xdg_dir(
                "SHORE_DATA_DIR",
                "XDG_DATA_HOME",
                dirs::data_dir,
                "~/.local/share",
            ),
            runtime: resolve_xdg_dir(
                "SHORE_RUNTIME_DIR",
                "XDG_RUNTIME_DIR",
                dirs::runtime_dir,
                "",
            ),
            cache: resolve_xdg_dir(
                "SHORE_CACHE_DIR",
                "XDG_CACHE_HOME",
                dirs::cache_dir,
                "~/.cache",
            ),
        }
    }
}

/// Convenience: resolved Shore config directory.
pub fn config_dir() -> PathBuf {
    ShoreDirs::resolve().config
}

/// Convenience: resolved Shore data directory.
pub fn data_dir() -> PathBuf {
    ShoreDirs::resolve().data
}

/// Convenience: resolved Shore runtime directory.
pub fn runtime_dir() -> PathBuf {
    ShoreDirs::resolve().runtime
}

/// Fully loaded daemon configuration.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub app: AppConfig,
    pub models: ModelCatalog,
    pub providers: ProviderRegistry,
    pub dirs: ShoreDirs,
    /// Raw global TOML table (after include/conf.d merging, before model extraction).
    /// Preserved for per-character config merging.
    raw_table: Option<toml::Table>,
}

impl LoadedConfig {
    /// Construct a `LoadedConfig` programmatically (no include/conf.d merging).
    ///
    /// Primarily useful for tests and integration harnesses.
    pub fn new_for_test(app: AppConfig, models: ModelCatalog, dirs: ShoreDirs) -> Self {
        Self {
            app,
            models,
            providers: ProviderRegistry::default(),
            dirs,
            raw_table: None,
        }
    }

    /// Access the raw TOML table for per-character merging.
    pub fn raw_table(&self) -> Option<&toml::Table> {
        self.raw_table.as_ref()
    }
}

pub const CHARACTER_WORKSPACE_DIR: &str = "workspace";
pub const SOUL_FILE: &str = "SOUL.md";
pub const USER_FILE: &str = "USER.md";
pub const AGENTS_FILE: &str = "AGENTS.md";
pub const TOOLS_FILE: &str = "TOOLS.md";
pub const HEARTBEAT_FILE: &str = "HEARTBEAT.md";
pub const MEMORY_DIR: &str = "memory";

/// Return `characters/{name}/`.
pub fn character_config_dir(config_dir: &Path, character_name: &str) -> PathBuf {
    config_dir.join("characters").join(character_name)
}

/// Return `characters/{name}/workspace/`.
pub fn character_workspace_dir(config_dir: &Path, character_name: &str) -> PathBuf {
    character_config_dir(config_dir, character_name).join(CHARACTER_WORKSPACE_DIR)
}

/// Return `characters/{name}/workspace/{name}`.
pub fn character_workspace_file(config_dir: &Path, character_name: &str, name: &str) -> PathBuf {
    character_workspace_dir(config_dir, character_name).join(name)
}

/// Return `characters/{name}/workspace/memory/`.
pub fn character_memory_dir(config_dir: &Path, character_name: &str) -> PathBuf {
    character_workspace_dir(config_dir, character_name).join(MEMORY_DIR)
}

/// Load and validate daemon configuration.
///
/// Resolution order:
/// 1. If `config_path` is provided, load config.toml from there.
/// 2. Otherwise, load from `$XDG_CONFIG_HOME/shore/config.toml`.
/// 3. If the file doesn't exist, use defaults.
///
/// The config is parsed in two phases:
/// 1. Parse as raw `toml::Table`, process `include` and `conf.d/`.
/// 2. Extract model sections (`chat`, `tools`, `embedding`, `image_generation`),
///    then deserialize the remainder into `AppConfig` (preserving `deny_unknown_fields`).
pub fn load_config(config_path: Option<&Path>) -> Result<LoadedConfig, ConfigError> {
    let mut dirs = ShoreDirs::resolve();

    // Determine the config directory (either from --config path or XDG).
    let config_dir = match config_path {
        Some(p) => {
            let dir = p.parent().unwrap_or(Path::new(".")).to_path_buf();
            // When a custom config path is provided, use its parent as the
            // config directory so that character lookups etc. are relative to it.
            dirs.config = dir.clone();
            dir
        }
        None => dirs.config.clone(),
    };

    let config_file = match config_path {
        Some(p) => p.to_path_buf(),
        None => config_dir.join("config.toml"),
    };

    // ── Load .env from config directory ───────────────────────────────
    let env_path = config_dir.join(".env");
    if env_path.exists() {
        match dotenvy::from_path_override(&env_path) {
            Ok(()) => info!(path = %env_path.display(), "Loaded .env file"),
            Err(e) => warn!(path = %env_path.display(), error = %e, "Failed to load .env file"),
        }
    }

    // ── Phase 1: Load raw TOML table ──────────────────────────────────
    let mut table: toml::Table = if config_file.exists() {
        let content = std::fs::read_to_string(&config_file).map_err(|e| ConfigError::ReadFile {
            path: config_file.clone(),
            source: e,
        })?;
        info!(path = %config_file.display(), "Loading config.toml");
        content
            .parse::<toml::Table>()
            .map_err(ConfigError::ParseApp)?
    } else {
        info!("No config.toml found, creating default config");
        create_default_config(&config_dir);
        toml::Table::new()
    };

    // ── Process `include = [...]` ─────────────────────────────────────
    if let Some(includes) = table.remove("include") {
        if let Some(arr) = includes.as_array() {
            for item in arr {
                if let Some(rel_path) = item.as_str() {
                    let include_path = config_dir.join(rel_path);
                    if include_path.exists() {
                        let content = std::fs::read_to_string(&include_path).map_err(|e| {
                            ConfigError::ReadFile {
                                path: include_path.clone(),
                                source: e,
                            }
                        })?;
                        let include_table: toml::Table =
                            content.parse().map_err(|e| ConfigError::ParseInclude {
                                path: include_path.clone(),
                                source: e,
                            })?;
                        info!(path = %include_path.display(), "Merging include file");
                        deep_merge(&mut table, &include_table);
                    } else {
                        warn!(path = %include_path.display(), "Include file not found, skipping");
                    }
                }
            }
        }
    }

    // ── Process conf.d/ ───────────────────────────────────────────────
    let conf_d = config_dir.join("conf.d");
    load_conf_d(&conf_d, &mut table)?;

    // Phase 2: parse the merged table into LoadedConfig.
    parse_config_table(table, dirs)
}

/// Parse a merged TOML table into a `LoadedConfig`.
///
/// Extracts model sections (`chat`, `tools`, `embedding`, `image_generation`),
/// deserializes the remainder into `AppConfig`, builds `ModelCatalog`, validates.
fn parse_config_table(table: toml::Table, dirs: ShoreDirs) -> Result<LoadedConfig, ConfigError> {
    // Preserve the raw table for per-character merging.
    let raw_table = table.clone();

    let mut table = table;
    let chat_section = table.remove("chat");
    let tools_section = table.remove("tools");
    let embedding_section = table.remove("embedding");
    let image_generation_section = table.remove("image_generation");
    let providers_section = table.remove("providers");

    // Deserialize the remaining table into AppConfig.
    let mut app: AppConfig = toml::Value::Table(table)
        .try_into()
        .map_err(ConfigError::ParseApp)?;

    // Forward legacy top-level `defaults.heartbeat` / `defaults.dreaming`
    // into `defaults.background.*` with a one-time deprecation warning.
    app.defaults.normalize_deprecated_aliases();

    // Build the provider registry first so the model catalog can
    // inherit registry-level transport defaults (sdk, base_url,
    // api_key_env) into static `[chat.<provider>]` entries.
    let providers =
        ProviderRegistry::from_section(providers_section.as_ref().and_then(|v| v.as_table()))?;

    let catalog = ModelCatalog::from_sections_with_providers(
        chat_section.as_ref().and_then(|v| v.as_table()),
        tools_section.as_ref().and_then(|v| v.as_table()),
        embedding_section.as_ref().and_then(|v| v.as_table()),
        image_generation_section.as_ref().and_then(|v| v.as_table()),
        Some(&providers),
    )?;

    validate_config(&app, &catalog)?;

    Ok(LoadedConfig {
        app,
        models: catalog,
        providers,
        dirs,
        raw_table: Some(raw_table),
    })
}

/// Load a per-character config overlay and deep-merge it over the global config.
///
/// Reads `{config_dir}/characters/{name}/config.toml`. If the file doesn't
/// exist, returns `Ok(None)`. If it does, deep-merges the character TOML
/// over the global raw table, then runs the full two-phase parse.
pub fn load_character_config(
    global: &LoadedConfig,
    character_name: &str,
) -> Result<Option<LoadedConfig>, ConfigError> {
    let config_dir = &global.dirs.config;
    let char_config_path = config_dir
        .join("characters")
        .join(character_name)
        .join("config.toml");

    if !char_config_path.exists() {
        return Ok(None);
    }

    let content =
        std::fs::read_to_string(&char_config_path).map_err(|e| ConfigError::ReadFile {
            path: char_config_path.clone(),
            source: e,
        })?;

    let char_table: toml::Table = content.parse().map_err(|e| ConfigError::ParseInclude {
        path: char_config_path.clone(),
        source: e,
    })?;

    info!(
        character = character_name,
        path = %char_config_path.display(),
        "Merging per-character config override"
    );

    // Clone the global raw table and deep-merge the character overlay.
    let base = global.raw_table.as_ref().cloned().unwrap_or_default();
    let mut merged = base;
    deep_merge(&mut merged, &char_table);

    parse_config_table(merged, global.dirs.clone()).map(Some)
}

/// Recursively deep-merge `overlay` into `base`.
///
/// For table values, recurse. For all other values, overlay overwrites base.
pub fn deep_merge(base: &mut toml::Table, overlay: &toml::Table) {
    for (key, overlay_val) in overlay {
        match (base.get_mut(key), overlay_val) {
            (Some(toml::Value::Table(base_sub)), toml::Value::Table(overlay_sub)) => {
                deep_merge(base_sub, overlay_sub);
            }
            _ => {
                base.insert(key.clone(), overlay_val.clone());
            }
        }
    }
}

/// Load and merge all `*.toml` files from a `conf.d/` directory, sorted alphabetically.
fn load_conf_d(dir: &Path, table: &mut toml::Table) -> Result<(), ConfigError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(()), // Directory doesn't exist — that's fine.
    };

    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    paths.sort();

    for path in paths {
        let content = std::fs::read_to_string(&path).map_err(|e| ConfigError::ReadFile {
            path: path.clone(),
            source: e,
        })?;
        let overlay: toml::Table = content.parse().map_err(|e| ConfigError::ConfD {
            path: path.clone(),
            source: e,
        })?;
        info!(path = %path.display(), "Merging conf.d file");
        deep_merge(table, &overlay);
    }

    Ok(())
}

/// Write a starter config.toml with commented options.
fn create_default_config(config_dir: &Path) {
    if let Err(e) = std::fs::create_dir_all(config_dir) {
        warn!(error = %e, "Could not create config directory");
        return;
    }
    let content = r#"# Shore V2 configuration
# See examples/config.toml for all available options.
#
# Characters are discovered from the characters/ directory.
# Create characters/<name>/workspace/SOUL.md to define a character.
#
# Models are configured inline under [chat.<provider>.<model>].
# You can also use `include = ["extra.toml"]` or conf.d/*.toml for modular config.

# include = ["models.toml"]  # optional explicit includes

# [defaults]
# model = "opus"              # must match a model key below

# [chat.anthropic]
# sdk = "anthropic"
# api_key_env = "ANTHROPIC_API_KEY"
#
# [chat.anthropic.sonnet]
# model_id = "claude-sonnet-4-6"

# [daemon]
# addr = "127.0.0.1:7320"
# unsafe_allow_remote_access = false  # required for non-loopback binds
# allowed_hosts = []                  # IP allowlist only; not auth/TLS

# [services.llm]
# command = "node /path/to/shore-llm/dist/index.js"
"#;
    let path = config_dir.join("config.toml");
    match std::fs::write(&path, content) {
        Ok(()) => info!(path = %path.display(), "Created default config.toml"),
        Err(e) => warn!(error = %e, "Could not write default config.toml"),
    }
}

/// Validate cross-field config constraints.
fn validate_config(app: &AppConfig, catalog: &ModelCatalog) -> Result<(), ConfigError> {
    // Validate model references exist in the catalog. heartbeat/dreaming
    // resolve against chat+tools models; embedding/image_generation resolve
    // against their respective profile maps. Without these checks a typo
    // silently falls back to defaults.model (heartbeat) or surfaces only at
    // tool-invocation time (dreaming/embedding/image_generation).
    validate_model_ref(catalog, "defaults.model", app.defaults.model.as_deref())?;
    validate_model_ref(
        catalog,
        "defaults.background.model",
        app.defaults.background.model.as_deref(),
    )?;
    validate_model_ref(
        catalog,
        "defaults.background.heartbeat",
        app.defaults.background.heartbeat.as_deref(),
    )?;
    validate_model_ref(
        catalog,
        "defaults.background.compaction",
        app.defaults.background.compaction.as_deref(),
    )?;
    validate_model_ref(
        catalog,
        "defaults.background.dreaming",
        app.defaults.background.dreaming.as_deref(),
    )?;
    validate_profile_ref(
        &catalog.embedding,
        "defaults.embedding",
        "embedding",
        app.defaults.embedding.as_deref(),
    )?;
    validate_profile_ref(
        &catalog.image_generation,
        "defaults.image_generation",
        "image_generation",
        app.defaults.image_generation.as_deref(),
    )?;

    validate_daily_cron(&app.memory.dreaming.frequency)?;

    Ok(())
}

/// Validate that an optional profile reference exists in a profile map.
fn validate_profile_ref(
    profiles: &std::collections::BTreeMap<String, toml::Value>,
    field: &str,
    catalog_section: &str,
    name: Option<&str>,
) -> Result<(), ConfigError> {
    if let Some(name) = name {
        if !profiles.contains_key(name) {
            return Err(ConfigError::Validation(format!(
                "{field} \"{name}\" not found in [{catalog_section}] profiles"
            )));
        }
    }
    Ok(())
}

fn validate_daily_cron(expr: &str) -> Result<(), ConfigError> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return Err(ConfigError::Validation(format!(
            "memory.dreaming.frequency must be a five-field cron expression, got {expr:?}"
        )));
    }
    let minute = parts[0].parse::<u8>().ok();
    let hour = parts[1].parse::<u8>().ok();
    if minute.is_none_or(|m| m > 59)
        || hour.is_none_or(|h| h > 23)
        || parts[2] != "*"
        || parts[3] != "*"
        || parts[4] != "*"
    {
        return Err(ConfigError::Validation(format!(
            "memory.dreaming.frequency currently supports daily cron of the form \"M H * * *\", got {expr:?}"
        )));
    }
    Ok(())
}

/// Validate that an optional model reference exists in the catalog.
fn validate_model_ref(
    catalog: &ModelCatalog,
    field: &str,
    name: Option<&str>,
) -> Result<(), ConfigError> {
    if let Some(name) = name {
        if catalog.find_model(name).is_err() {
            return Err(ConfigError::Validation(format!(
                "{field} \"{name}\" not found in model catalog"
            )));
        }
    }
    Ok(())
}

/// Load character definition from `characters/{name}/workspace/SOUL.md`,
/// with a legacy fallback to `character.md`.
pub fn load_character_definition(config_dir: &Path, character_name: &str) -> Option<String> {
    let new_path = character_workspace_file(config_dir, character_name, SOUL_FILE);
    if let Ok(content) = std::fs::read_to_string(&new_path) {
        info!(character = character_name, path = %new_path.display(), "Loaded character definition");
        return Some(content);
    }

    let legacy_path = character_config_dir(config_dir, character_name).join("character.md");
    match std::fs::read_to_string(&legacy_path) {
        Ok(content) => {
            info!(character = character_name, path = %legacy_path.display(), "Loaded legacy character definition");
            Some(content)
        }
        Err(_) => {
            warn!(
                character = character_name,
                path = %new_path.display(),
                "No character definition found"
            );
            None
        }
    }
}

/// Resolve user definition from `characters/{name}/workspace/USER.md`, with a
/// legacy fallback to `characters/{name}/user.md`.
pub fn resolve_user_definition(config_dir: &Path, character_name: &str) -> Option<String> {
    let workspace_user = character_workspace_file(config_dir, character_name, USER_FILE);
    if let Ok(content) = std::fs::read_to_string(&workspace_user) {
        info!(
            character = character_name,
            "Using character-specific user definition"
        );
        return Some(content);
    }

    let legacy_user = character_config_dir(config_dir, character_name).join("user.md");
    if let Ok(content) = std::fs::read_to_string(&legacy_user) {
        info!(
            character = character_name,
            "Using legacy character-specific user definition"
        );
        return Some(content);
    }

    None
}

/// Discover available characters by scanning `characters/` directory.
///
/// Returns the names of all subdirectories under `{config_dir}/characters/`
/// that contain either `workspace/SOUL.md` or the legacy `character.md`.
pub fn discover_characters(config_dir: &Path) -> Vec<String> {
    let chars_dir = config_dir.join("characters");
    let entries = match std::fs::read_dir(&chars_dir) {
        Ok(entries) => entries,
        Err(_) => return vec![],
    };

    let mut names = Vec::new();
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            if entry
                .path()
                .join(CHARACTER_WORKSPACE_DIR)
                .join(SOUL_FILE)
                .exists()
                || entry.path().join("character.md").exists()
            {
                names.push(name);
            }
        }
    }
    names.sort();
    names
}

/// Resolve a prompt template: character-specific → global → None.
///
/// Returns the template content if found, or None if no override exists
/// (caller should fall back to built-in default).
pub fn resolve_prompt_template(
    config_dir: &Path,
    character_name: &str,
    template_name: &str,
) -> Option<String> {
    // 1. Character-specific prompt override.
    let char_prompt = config_dir
        .join("characters")
        .join(character_name)
        .join("prompts")
        .join(template_name);
    if let Ok(content) = std::fs::read_to_string(&char_prompt) {
        return Some(content);
    }

    // 2. Global prompt.
    let global_prompt = config_dir.join("prompts").join(template_name);
    if let Ok(content) = std::fs::read_to_string(&global_prompt) {
        return Some(content);
    }

    // 3. Caller provides built-in default.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a temp config directory with given files.
    fn setup_config_dir(files: &[(&str, &str)]) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let full_path = tmp.path().join(path);
            if let Some(parent) = full_path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&full_path, content).unwrap();
        }
        tmp
    }

    #[test]
    fn load_unified_config() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[daemon]
addr = "127.0.0.1:9999"
allowed_hosts = ["127.0.0.1"]

[behavior.autonomy]
enabled = true

[behavior.autonomy.heartbeat]
enabled = false
fallback_heartbeat_interval = "30m"

[behavior.tool_use.tools]
roll_dice = false

[advanced]
max_retries = 5

[chat.anthropic]
api_key_env = "MY_KEY"

[chat.anthropic.sonnet]
model_id = "claude-sonnet-4-6"

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        )]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();

        assert_eq!(loaded.app.daemon.addr, "127.0.0.1:9999");
        assert_eq!(loaded.app.daemon.allowed_hosts, vec!["127.0.0.1"]);
        assert!(loaded.app.behavior.autonomy.enabled);
        assert!(!loaded.app.behavior.autonomy.heartbeat.enabled);
        assert_eq!(
            loaded
                .app
                .behavior
                .autonomy
                .heartbeat
                .fallback_heartbeat_interval,
            ConfigDuration::from_secs(1800)
        );
        assert!(!loaded.app.behavior.tool_use.tools.roll_dice());
        assert!(loaded.app.behavior.tool_use.tools.memory());
        assert_eq!(loaded.app.advanced.max_retries, Some(5));
        assert!(!loaded.app.daemon.unsafe_allow_remote_access);

        assert_eq!(loaded.models.chat.len(), 2);
        assert!(loaded.models.find_model("sonnet").is_ok());
        assert!(loaded.models.find_model("opus").is_ok());
    }

    #[test]
    fn missing_optional_fields_get_defaults() {
        let tmp = setup_config_dir(&[("config.toml", "")]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();

        // All defaults should be filled in.
        assert!(loaded.app.defaults.stream);
        assert!(loaded.app.defaults.dreaming.is_none());
        assert!(!loaded.app.behavior.autonomy.enabled);
        assert!(loaded.app.behavior.autonomy.heartbeat.enabled);
        assert_eq!(
            loaded
                .app
                .behavior
                .autonomy
                .heartbeat
                .fallback_heartbeat_interval,
            ConfigDuration::from_secs(3600)
        );
        assert!(loaded.app.behavior.tool_use.enabled);
        assert!(loaded.app.memory.compaction.enabled);
        assert!(!loaded.app.memory.dreaming.enabled);
        assert_eq!(loaded.app.memory.dreaming.frequency, "0 3 * * *");
        assert_eq!(loaded.app.memory.dreaming.max_tool_rounds, 12);
        assert_eq!(loaded.app.daemon.addr, "127.0.0.1:7320"); // default
        assert!(!loaded.app.daemon.unsafe_allow_remote_access);
        assert!(loaded.app.advanced.editor.is_none());
        assert!(loaded.app.advanced.max_retries.is_none());
        assert!(loaded.app.advanced.retry_backoff.is_none());
    }

    #[test]
    fn invalid_dreaming_frequency_fails_validation() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[memory.dreaming]
frequency = "sometimes"
"#,
        )]);

        let config_path = tmp.path().join("config.toml");
        let err = load_config(Some(&config_path)).unwrap_err();
        assert!(
            err.to_string().contains("memory.dreaming.frequency"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn invalid_config_produces_clear_error_unknown_section() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[completely_unknown]
key = "value"
"#,
        )]);

        let config_path = tmp.path().join("config.toml");
        let err = load_config(Some(&config_path)).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown field"),
            "Error should mention unknown field: {msg}"
        );
    }

    #[test]
    fn model_sections_dont_trigger_unknown_field_errors() {
        // This is the key two-phase parse test: model sections are extracted
        // before AppConfig deserialization, so they don't cause errors.
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[chat.anthropic.opus]
model_id = "claude-opus-4-6"

[tools.openrouter.mistral]
model_id = "mistralai/mistral-small"

[embedding.text-large]
model_id = "openai/text-embedding-3-large"

[image_generation.gemini-flash]
model_id = "google/gemini-flash"
"#,
        )]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        assert!(loaded.models.find_model("opus").is_ok());
        assert!(loaded.models.find_model("mistral").is_ok());
        assert!(loaded.models.embedding.contains_key("text-large"));
        assert!(loaded.models.image_generation.contains_key("gemini-flash"));
    }

    #[test]
    fn invalid_default_model_reference() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[defaults]
model = "nonexistent-model"
"#,
        )]);

        let config_path = tmp.path().join("config.toml");
        let err = load_config(Some(&config_path)).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent-model"),
            "Should mention the missing model: {msg}"
        );
    }

    #[test]
    fn invalid_default_heartbeat_reference() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[defaults]
heartbeat = "ghost-haiku"

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        )]);
        let err = load_config(Some(&tmp.path().join("config.toml"))).unwrap_err();
        let msg = err.to_string();
        // Legacy `defaults.heartbeat` is forwarded to `defaults.background.heartbeat`
        // at parse time, so validation surfaces the new path.
        assert!(msg.contains("defaults.background.heartbeat"), "{msg}");
        assert!(msg.contains("ghost-haiku"), "{msg}");
    }

    #[test]
    fn invalid_default_dreaming_reference() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[defaults]
dreaming = "no-such-model"

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        )]);
        let err = load_config(Some(&tmp.path().join("config.toml"))).unwrap_err();
        assert!(err.to_string().contains("defaults.background.dreaming"));
    }

    #[test]
    fn invalid_default_embedding_reference() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[defaults]
embedding = "missing-profile"

[embedding.text-large]
model_id = "openai/text-embedding-3-large"
"#,
        )]);
        let err = load_config(Some(&tmp.path().join("config.toml"))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("defaults.embedding"), "{msg}");
        assert!(msg.contains("missing-profile"), "{msg}");
    }

    #[test]
    fn invalid_default_image_generation_reference() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[defaults]
image_generation = "missing-profile"

[image_generation.gemini-flash]
model_id = "google/gemini-flash"
"#,
        )]);
        let err = load_config(Some(&tmp.path().join("config.toml"))).unwrap_err();
        assert!(err.to_string().contains("defaults.image_generation"));
    }

    #[test]
    fn valid_defaults_pass_validation() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[defaults]
model = "opus"
heartbeat = "opus"
dreaming = "opus"
embedding = "text-large"
image_generation = "gemini-flash"

[chat.anthropic.opus]
model_id = "claude-opus-4-6"

[embedding.text-large]
model_id = "openai/text-embedding-3-large"

[image_generation.gemini-flash]
model_id = "google/gemini-flash"
"#,
        )]);
        load_config(Some(&tmp.path().join("config.toml"))).unwrap();
    }

    #[test]
    fn no_config_files_uses_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        assert_eq!(loaded.app, AppConfig::default());
        assert!(loaded.models.chat.is_empty());
    }

    // ── Include tests ─────────────────────────────────────────────────

    #[test]
    fn include_files_merged() {
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
include = ["models.toml"]

[defaults]
model = "opus"
"#,
            ),
            (
                "models.toml",
                r#"
[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        assert!(loaded.models.find_model("opus").is_ok());
    }

    #[test]
    fn missing_include_file_is_warning_not_error() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
include = ["nonexistent.toml"]
"#,
        )]);

        let config_path = tmp.path().join("config.toml");
        // Should succeed — missing include is a warning, not an error.
        let loaded = load_config(Some(&config_path)).unwrap();
        assert!(loaded.models.chat.is_empty());
    }

    // ── conf.d tests ──────────────────────────────────────────────────

    #[test]
    fn conf_d_files_merged_alphabetically() {
        let tmp = setup_config_dir(&[
            ("config.toml", ""),
            (
                "conf.d/01-chat.toml",
                r#"
[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
            ),
            (
                "conf.d/02-tools.toml",
                r#"
[tools.openrouter.mistral]
model_id = "mistralai/mistral-small"
"#,
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        assert!(loaded.models.find_model("opus").is_ok());
        assert!(loaded.models.find_model("mistral").is_ok());
    }

    #[test]
    fn conf_d_overrides_base_config() {
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
[chat.anthropic]
api_key_env = "BASE_KEY"

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
            ),
            (
                "conf.d/override.toml",
                r#"
[chat.anthropic]
api_key_env = "OVERRIDE_KEY"
"#,
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        let opus = loaded.models.find_model("opus").unwrap();
        assert_eq!(opus.api_key_env.as_deref(), Some("OVERRIDE_KEY"));
    }

    #[test]
    fn conf_d_nonexistent_dir_is_fine() {
        let tmp = setup_config_dir(&[("config.toml", "")]);
        // No conf.d/ directory — should not error.
        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        assert!(loaded.models.chat.is_empty());
    }

    #[test]
    fn merge_order_conf_d_over_include_over_base() {
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
include = ["include.toml"]

[chat.anthropic]
temperature = 0.1

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
            ),
            (
                "include.toml",
                r#"
[chat.anthropic]
temperature = 0.5
"#,
            ),
            (
                "conf.d/final.toml",
                r#"
[chat.anthropic]
temperature = 0.9
"#,
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        let opus = loaded.models.find_model("opus").unwrap();
        // conf.d overrides include which overrides base.
        assert_eq!(opus.temperature, Some(0.9));
    }

    // ── Deep merge ────────────────────────────────────────────────────

    #[test]
    fn deep_merge_scalars_overwritten() {
        let mut base = r#"key = "a""#.parse::<toml::Table>().unwrap();
        let overlay = r#"key = "b""#.parse::<toml::Table>().unwrap();
        deep_merge(&mut base, &overlay);
        assert_eq!(base["key"].as_str(), Some("b"));
    }

    #[test]
    fn deep_merge_tables_recursive() {
        let mut base = r#"
[section]
a = 1
b = 2
"#
        .parse::<toml::Table>()
        .unwrap();

        let overlay = r#"
[section]
b = 3
c = 4
"#
        .parse::<toml::Table>()
        .unwrap();

        deep_merge(&mut base, &overlay);
        let section = base["section"].as_table().unwrap();
        assert_eq!(section["a"].as_integer(), Some(1)); // preserved
        assert_eq!(section["b"].as_integer(), Some(3)); // overwritten
        assert_eq!(section["c"].as_integer(), Some(4)); // added
    }

    // ── Character / prompt tests ──────────────────────────────────────

    #[test]
    fn character_definition_loaded() {
        let tmp = setup_config_dir(&[(
            "characters/TestChar/workspace/SOUL.md",
            "You are TestChar, a helpful assistant.",
        )]);

        let def = load_character_definition(tmp.path(), "TestChar");
        assert_eq!(
            def.as_deref(),
            Some("You are TestChar, a helpful assistant.")
        );
    }

    #[test]
    fn user_definition_character_specific_overrides_global() {
        let tmp = setup_config_dir(&[(
            "characters/TestChar/workspace/USER.md",
            "Character-specific user definition",
        )]);

        let def = resolve_user_definition(tmp.path(), "TestChar");
        assert_eq!(def.as_deref(), Some("Character-specific user definition"));
    }

    #[test]
    fn user_definition_uses_legacy_character_file() {
        let tmp = setup_config_dir(&[(
            "characters/TestChar/user.md",
            "Legacy character-specific user definition",
        )]);

        let def = resolve_user_definition(tmp.path(), "TestChar");
        assert_eq!(
            def.as_deref(),
            Some("Legacy character-specific user definition")
        );
    }

    #[test]
    fn discover_characters_finds_valid_chars() {
        let tmp = setup_config_dir(&[
            ("characters/Alice/workspace/SOUL.md", "Alice character"),
            ("characters/Bob/character.md", "Bob character"),
            ("characters/EmptyDir/.gitkeep", ""), // no character.md
        ]);

        let chars = discover_characters(tmp.path());
        assert_eq!(chars, vec!["Alice", "Bob"]);
    }

    #[test]
    fn prompt_template_resolution_order() {
        let tmp = setup_config_dir(&[
            (
                "characters/TestChar/prompts/system.md",
                "Character system prompt",
            ),
            ("prompts/system.md", "Global system prompt"),
            ("prompts/compact.md", "Global compact prompt"),
        ]);

        // Character-specific wins.
        let result = resolve_prompt_template(tmp.path(), "TestChar", "system.md");
        assert_eq!(result.as_deref(), Some("Character system prompt"));

        // Falls back to global.
        let result = resolve_prompt_template(tmp.path(), "TestChar", "compact.md");
        assert_eq!(result.as_deref(), Some("Global compact prompt"));

        // Returns None if no file exists.
        let result = resolve_prompt_template(tmp.path(), "TestChar", "nonexistent.md");
        assert!(result.is_none());
    }

    #[test]
    fn xdg_dirs_resolve() {
        let dirs = ShoreDirs::resolve();
        // Should end in /shore for all paths.
        assert!(dirs.config.ends_with("shore"));
        assert!(dirs.data.ends_with("shore"));
        assert!(dirs.runtime.ends_with("shore"));
    }

    // ── Per-character config override tests ───────────────────────────

    #[test]
    fn character_config_override_merges_over_global() {
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
[defaults]
model = "sonnet"

[behavior.autonomy]
enabled = false

[behavior.autonomy.heartbeat]
fallback_heartbeat_interval = "1h"

[chat.anthropic.sonnet]
model_id = "claude-sonnet-4-6"

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
            ),
            ("characters/Alice/character.md", "You are Alice."),
            (
                "characters/Alice/config.toml",
                r#"
[defaults]
model = "opus"

[behavior.autonomy]
enabled = true

[behavior.autonomy.heartbeat]
fallback_heartbeat_interval = "30m"
"#,
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let global = load_config(Some(&config_path)).unwrap();

        // Global config should be unchanged.
        assert_eq!(global.app.defaults.model.as_deref(), Some("sonnet"));
        assert!(!global.app.behavior.autonomy.enabled);
        assert_eq!(
            global
                .app
                .behavior
                .autonomy
                .heartbeat
                .fallback_heartbeat_interval,
            ConfigDuration::from_secs(3600)
        );

        // Character config should override specific keys.
        let alice = load_character_config(&global, "Alice").unwrap().unwrap();
        assert_eq!(alice.app.defaults.model.as_deref(), Some("opus"));
        assert!(alice.app.behavior.autonomy.enabled);
        assert_eq!(
            alice
                .app
                .behavior
                .autonomy
                .heartbeat
                .fallback_heartbeat_interval,
            ConfigDuration::from_secs(1800)
        );

        // Models should still be available (inherited from global).
        assert!(alice.models.find_model("sonnet").is_ok());
        assert!(alice.models.find_model("opus").is_ok());
    }

    #[test]
    fn character_config_no_override_returns_none() {
        let tmp = setup_config_dir(&[
            ("config.toml", ""),
            ("characters/Bob/character.md", "You are Bob."),
        ]);

        let config_path = tmp.path().join("config.toml");
        let global = load_config(Some(&config_path)).unwrap();

        assert!(load_character_config(&global, "Bob").unwrap().is_none());
    }

    #[test]
    fn character_config_adds_models() {
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
[chat.anthropic.sonnet]
model_id = "claude-sonnet-4-6"
"#,
            ),
            ("characters/Alice/character.md", "You are Alice."),
            (
                "characters/Alice/config.toml",
                r#"
[defaults]
model = "opus"

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
max_tokens = 16384
"#,
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let global = load_config(Some(&config_path)).unwrap();
        assert!(global.models.find_model("opus").is_err());

        let alice = load_character_config(&global, "Alice").unwrap().unwrap();
        assert!(alice.models.find_model("sonnet").is_ok());
        assert!(alice.models.find_model("opus").is_ok());
        assert_eq!(
            alice.models.find_model("opus").unwrap().max_tokens,
            Some(16384)
        );
    }

    // ── .env loading tests ────────────────────────────────────────────

    #[test]
    fn dotenv_file_loaded_into_env() {
        let tmp = setup_config_dir(&[
            ("config.toml", ""),
            (".env", "SHORE_TEST_DOTENV_VAR_1234=hello_from_dotenv"),
        ]);

        let config_path = tmp.path().join("config.toml");
        let _loaded = load_config(Some(&config_path)).unwrap();

        assert_eq!(
            std::env::var("SHORE_TEST_DOTENV_VAR_1234").ok().as_deref(),
            Some("hello_from_dotenv"),
        );
        // Clean up.
        std::env::remove_var("SHORE_TEST_DOTENV_VAR_1234");
    }

    #[test]
    fn no_dotenv_file_is_fine() {
        // No .env file — load_config should still succeed.
        let tmp = setup_config_dir(&[("config.toml", "")]);
        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path));
        assert!(loaded.is_ok());
    }

    // ── create_default_config tests ─────────────────────────────────

    #[test]
    fn create_default_config_creates_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("newdir");
        // Directory doesn't exist yet — create_default_config should create it.
        create_default_config(&sub);

        let config_path = sub.join("config.toml");
        assert!(config_path.exists(), "config.toml should be created");

        let content = std::fs::read_to_string(&config_path).unwrap();
        assert!(content.contains("Shore V2 configuration"));
        assert!(content.contains("[defaults]"));
        assert!(content.contains("[chat.anthropic]"));
    }

    #[test]
    fn create_default_config_via_load_when_missing() {
        // load_config with no existing config.toml should create default.
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        assert!(!config_path.exists());

        let loaded = load_config(Some(&config_path)).unwrap();
        // Should produce defaults (empty app config).
        assert!(loaded.app.defaults.model.is_none());

        // The default config should now exist on disk.
        assert!(config_path.exists());
    }

    // ── XDG override env var tests ──────────────────────────────────

    #[test]
    fn xdg_override_shore_config_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let custom = tmp.path().join("my_config");
        std::fs::create_dir_all(&custom).unwrap();

        // Set SHORE_CONFIG_DIR override — should be used as-is (no "/shore" suffix).
        std::env::set_var("SHORE_CONFIG_DIR", &custom);
        let dir = resolve_xdg_dir(
            "SHORE_CONFIG_DIR",
            "XDG_CONFIG_HOME",
            dirs::config_dir,
            "~/.config",
        );
        std::env::remove_var("SHORE_CONFIG_DIR");

        assert_eq!(dir, custom, "SHORE_CONFIG_DIR should be used as-is");
    }

    #[test]
    fn xdg_fallback_appends_shore() {
        // No override set — fallback path should have /shore appended.
        let unique = format!("SHORE_TEST_NO_OVERRIDE_{}", std::process::id());
        let xdg_unique = format!("SHORE_TEST_XDG_NO_{}", std::process::id());
        // Ensure neither env var is set.
        std::env::remove_var(&unique);
        std::env::remove_var(&xdg_unique);

        let dir = resolve_xdg_dir(
            &unique,
            &xdg_unique,
            || None, // no platform dir
            "/tmp/shore_fallback_test",
        );
        assert_eq!(dir, PathBuf::from("/tmp/shore_fallback_test/shore"));
    }

    #[test]
    fn xdg_empty_fallback_uses_temp_dir() {
        let unique = format!("SHORE_TEST_EMPTY_{}", std::process::id());
        let xdg_unique = format!("SHORE_TEST_XDG_EMPTY_{}", std::process::id());
        std::env::remove_var(&unique);
        std::env::remove_var(&xdg_unique);

        let dir = resolve_xdg_dir(&unique, &xdg_unique, || None, "");
        // Should use std::env::temp_dir() + "/shore"
        assert!(dir.ends_with("shore"));
        assert!(dir.parent().unwrap().exists(), "parent should be temp_dir");
    }

    #[test]
    fn character_config_with_conf_d_models() {
        // Simulates: global config defines model in conf.d, character overrides default model.
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
[defaults]
model = "kimi"
"#,
            ),
            (
                "conf.d/models.toml",
                r#"
[chat.anthropic.opus]
model_id = "claude-opus-4-6"

[chat.openrouter.kimi]
model_id = "kimi-k2"
"#,
            ),
            ("characters/qifei/character.md", "You are qifei."),
            (
                "characters/qifei/config.toml",
                r#"
[defaults]
model = "chat.anthropic.opus"
"#,
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let global = load_config(Some(&config_path)).unwrap();
        assert_eq!(global.app.defaults.model.as_deref(), Some("kimi"));

        let qifei = load_character_config(&global, "qifei").unwrap().unwrap();
        assert_eq!(
            qifei.app.defaults.model.as_deref(),
            Some("chat.anthropic.opus")
        );
        // conf.d models should still be available after merge.
        assert!(qifei.models.find_model("opus").is_ok());
        assert!(qifei.models.find_model("kimi").is_ok());
    }

    // ── Provider registry integration ────────────────────────────────

    #[test]
    fn empty_config_has_empty_provider_registry() {
        let tmp = setup_config_dir(&[("config.toml", "")]);
        let loaded = load_config(Some(&tmp.path().join("config.toml"))).unwrap();
        assert!(loaded.providers.is_empty());
    }

    #[test]
    fn existing_static_chat_config_loads_with_no_providers_section() {
        // Regression guard: static `[chat.X.Y]` config must continue to work
        // exactly as before, with an empty provider registry alongside it.
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[chat.anthropic.opus]
model_id = "claude-opus-4-6"

[chat.anthropic.sonnet]
model_id = "claude-sonnet-4-6"
"#,
        )]);
        let loaded = load_config(Some(&tmp.path().join("config.toml"))).unwrap();
        assert!(loaded.providers.is_empty());
        assert!(loaded.models.find_model("opus").is_ok());
        assert!(loaded.models.find_model("sonnet").is_ok());
    }

    #[test]
    fn provider_registry_loads_alongside_static_chat() {
        // Phase 1 deliverable: both can coexist; the registry parses but
        // does not yet alter resolution.
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[providers.openrouter]
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"

[[providers.openrouter.keys]]
name = "budget"
env = "OPENROUTER_API_KEY_BUDGET"
warn_on_fallback = true

[[providers.openrouter.keys]]
name = "overflow"
env = "OPENROUTER_API_KEY_OVERFLOW"

[providers.openrouter.discovery]
enabled = true

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        )]);
        let loaded = load_config(Some(&tmp.path().join("config.toml"))).unwrap();

        let or = loaded.providers.get("openrouter").unwrap();
        assert!(or.enabled);
        assert_eq!(or.base_url.as_deref(), Some("https://openrouter.ai/api/v1"));
        assert_eq!(or.keys.len(), 2);
        assert_eq!(or.keys[0].name, "budget");
        assert!(or.discovery.enabled);

        // Static catalog continues to work.
        assert!(loaded.models.find_model("opus").is_ok());
    }

    #[test]
    fn compact_api_key_env_form_loads() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[providers.openai]
api_key_env = "OPENAI_API_KEY"
"#,
        )]);
        let loaded = load_config(Some(&tmp.path().join("config.toml"))).unwrap();
        let p = loaded.providers.get("openai").unwrap();
        assert_eq!(p.keys.len(), 1, "compact form folded into a single key");
        assert_eq!(p.keys[0].name, "default");
        assert_eq!(p.keys[0].env, "OPENAI_API_KEY");
    }

    #[test]
    fn provider_registry_via_conf_d() {
        // Provider sections must merge through conf.d like every other
        // top-level section.
        let tmp = setup_config_dir(&[
            ("config.toml", ""),
            (
                "conf.d/providers.toml",
                r#"
[providers.openrouter]
sdk = "openai"

[[providers.openrouter.keys]]
name = "main"
env = "OR_KEY"
"#,
            ),
        ]);
        let loaded = load_config(Some(&tmp.path().join("config.toml"))).unwrap();
        assert!(loaded.providers.get("openrouter").is_some());
    }

    #[test]
    fn provider_registry_parse_error_propagates() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[providers.openrouter]
api_key_env = "A"

[[providers.openrouter.keys]]
name = "explicit"
env = "B"
"#,
        )]);
        let err = load_config(Some(&tmp.path().join("config.toml"))).unwrap_err();
        assert!(matches!(err, ConfigError::ProviderRegistry(_)));
        assert!(err.to_string().contains("api_key_env"));
    }

    #[test]
    fn legacy_chat_provider_api_key_env_unchanged() {
        // Existing single-key behavior under [chat.<provider>] must still
        // cascade into ResolvedModel.api_key_env. Phase 1 does not touch
        // this code path.
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[chat.anthropic]
api_key_env = "MY_LEGACY_KEY"

[chat.anthropic.opus]
model_id = "claude-opus-4-6"
"#,
        )]);
        let loaded = load_config(Some(&tmp.path().join("config.toml"))).unwrap();
        let opus = loaded.models.find_model("opus").unwrap();
        assert_eq!(opus.api_key_env.as_deref(), Some("MY_LEGACY_KEY"));
    }

    /// `examples/config.toml` is the documented entry point for new
    /// users. Drift between it and the parser is a real regression, so
    /// load it through the full two-phase loader and assert the bundled
    /// example reaches a valid `LoadedConfig`. New commented-out
    /// snippets in the example file are exercised via the per-section
    /// tests below.
    #[test]
    fn bundled_example_config_parses() {
        const EXAMPLE: &str = include_str!("../../../examples/config.toml");
        let tmp = setup_config_dir(&[("config.toml", EXAMPLE)]);
        let loaded = load_config(Some(&tmp.path().join("config.toml")))
            .expect("examples/config.toml must parse end-to-end");
        // The committed example defines [defaults].model = "claude-sonnet"
        // and the matching [chat.anthropic.claude-sonnet] entry. Both
        // sides being live is what protects users from copy/paste rot.
        assert_eq!(loaded.app.defaults.model.as_deref(), Some("claude-sonnet"));
        assert!(loaded.models.find_model("claude-sonnet").is_ok());
    }

    /// The Phase 9 OpenRouter budget/overflow + discovery snippet
    /// documented in `examples/config.toml` must continue to parse,
    /// even though it is commented out by default. Inline a copy here
    /// (uncommented) so an accidental rename of a parser field is
    /// caught immediately.
    #[test]
    fn documented_provider_snippet_parses() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[providers.openai]
api_key_env = "OPENAI_API_KEY"

[providers.openrouter]
enabled = true
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"

[[providers.openrouter.keys]]
name = "budget"
env = "OPENROUTER_API_KEY_BUDGET"
warn_on_fallback = true

[[providers.openrouter.keys]]
name = "overflow"
env = "OPENROUTER_API_KEY_OVERFLOW"

[providers.openrouter.discovery]
enabled = true
visibility = [
  "*",
  "!anthropic/*",
  "!openai/*",
  "!google/gemini-*",
]

[chat.openrouter.sonnet]
model_id = "anthropic/claude-sonnet-4.5"
cache_ttl = "1h"
max_tokens = 16384

[defaults]
model = "sonnet"
"#,
        )]);
        let loaded = load_config(Some(&tmp.path().join("config.toml"))).unwrap();

        // Provider registry shape.
        let or = loaded
            .providers
            .get("openrouter")
            .expect("openrouter present");
        assert!(or.enabled);
        assert_eq!(or.keys.len(), 2);
        assert_eq!(or.keys[0].name, "budget");
        assert!(or.keys[0].warn_on_fallback);
        assert_eq!(or.keys[1].name, "overflow");
        assert!(or.discovery.enabled);
        assert_eq!(or.discovery.visibility.len(), 4);

        // Compact form folds into a synthetic "default" key.
        let openai = loaded.providers.get("openai").unwrap();
        assert_eq!(openai.keys.len(), 1);
        assert_eq!(openai.keys[0].name, "default");

        // Static alias still drives [defaults].model validation.
        let sonnet = loaded.models.find_model("sonnet").unwrap();
        assert_eq!(sonnet.model_id, "anthropic/claude-sonnet-4.5");
        assert_eq!(sonnet.cache_ttl.as_deref(), Some("1h"));
    }
}
