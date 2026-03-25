pub mod app;
pub mod models;

use std::path::{Path, PathBuf};

use app::AppConfig;
use models::ModelsConfig;
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

    #[error("failed to parse models.toml: {0}")]
    ParseModels(#[source] toml::de::Error),

    #[error("validation error: {0}")]
    Validation(String),
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
}

impl ShoreDirs {
    /// Resolve XDG directories using environment variables or platform defaults.
    pub fn resolve() -> Self {
        let config = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(dirs::config_dir)
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join("shore");

        let data = std::env::var("XDG_DATA_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(dirs::data_dir)
            .unwrap_or_else(|| PathBuf::from("~/.local/share"))
            .join("shore");

        let runtime = std::env::var("XDG_RUNTIME_DIR")
            .ok()
            .map(PathBuf::from)
            .or_else(dirs::runtime_dir)
            .unwrap_or_else(std::env::temp_dir)
            .join("shore");

        Self {
            config,
            data,
            runtime,
        }
    }
}

/// Fully loaded daemon configuration.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub app: AppConfig,
    pub models: ModelsConfig,
    pub dirs: ShoreDirs,
    pub character_definition: Option<String>,
    pub user_definition: Option<String>,
}

/// Load and validate daemon configuration.
///
/// Resolution order:
/// 1. If `config_path` is provided, load config.toml from there.
/// 2. Otherwise, load from `$XDG_CONFIG_HOME/shore/config.toml`.
/// 3. If the file doesn't exist, use defaults.
///
/// models.toml is always loaded from the same directory as config.toml.
pub fn load_config(config_path: Option<&Path>) -> Result<LoadedConfig, ConfigError> {
    let dirs = ShoreDirs::resolve();

    // Determine the config directory (either from --config path or XDG).
    let config_dir = match config_path {
        Some(p) => p
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf(),
        None => dirs.config.clone(),
    };

    let config_file = match config_path {
        Some(p) => p.to_path_buf(),
        None => config_dir.join("config.toml"),
    };

    // ── Load config.toml ────────────────────────────────────────────
    let app: AppConfig = if config_file.exists() {
        let content = std::fs::read_to_string(&config_file).map_err(|e| ConfigError::ReadFile {
            path: config_file.clone(),
            source: e,
        })?;
        info!(path = %config_file.display(), "Loading config.toml");
        toml::from_str(&content).map_err(ConfigError::ParseApp)?
    } else {
        info!("No config.toml found, using defaults");
        AppConfig::default()
    };

    // ── Load models.toml ────────────────────────────────────────────
    let models_file = config_dir.join("models.toml");
    let models: ModelsConfig = if models_file.exists() {
        let content =
            std::fs::read_to_string(&models_file).map_err(|e| ConfigError::ReadFile {
                path: models_file.clone(),
                source: e,
            })?;
        info!(path = %models_file.display(), "Loading models.toml");
        toml::from_str(&content).map_err(ConfigError::ParseModels)?
    } else {
        info!("No models.toml found, using empty model list");
        ModelsConfig::default()
    };

    // ── Validate ────────────────────────────────────────────────────
    validate_config(&app, &models)?;

    // ── Load character definition ───────────────────────────────────
    let character_name = &app.character.name;
    let character_definition =
        load_character_definition(&config_dir, character_name);

    // ── Resolve user definition ─────────────────────────────────────
    let user_definition = resolve_user_definition(&config_dir, character_name);

    Ok(LoadedConfig {
        app,
        models,
        dirs,
        character_definition,
        user_definition,
    })
}

/// Validate cross-field config constraints.
fn validate_config(app: &AppConfig, models: &ModelsConfig) -> Result<(), ConfigError> {
    // Personality must be 0.0–1.0.
    let p = app.behavior.autonomy.personality;
    if !(0.0..=1.0).contains(&p) {
        return Err(ConfigError::Validation(format!(
            "behavior.autonomy.personality must be 0.0–1.0, got {p}"
        )));
    }

    // If a default model is specified, it must exist in models.toml.
    if let Some(ref default_model) = app.defaults.model {
        if models.find_model(default_model).is_none() {
            return Err(ConfigError::Validation(format!(
                "defaults.model \"{default_model}\" not found in models.toml"
            )));
        }
    }

    // Model names must be unique.
    let mut seen = std::collections::HashSet::new();
    for model in &models.models {
        if !seen.insert(&model.name) {
            return Err(ConfigError::Validation(format!(
                "duplicate model name \"{}\" in models.toml",
                model.name
            )));
        }
    }

    Ok(())
}

/// Load character definition from `characters/{name}/character.md`.
fn load_character_definition(config_dir: &Path, character_name: &str) -> Option<String> {
    let path = config_dir
        .join("characters")
        .join(character_name)
        .join("character.md");
    match std::fs::read_to_string(&path) {
        Ok(content) => {
            info!(character = character_name, "Loaded character definition");
            Some(content)
        }
        Err(_) => {
            warn!(
                character = character_name,
                path = %path.display(),
                "No character definition found"
            );
            None
        }
    }
}

/// Resolve user definition: character-specific user.md → global user.md.
fn resolve_user_definition(config_dir: &Path, character_name: &str) -> Option<String> {
    // 1. Character-specific user.md
    let char_user = config_dir
        .join("characters")
        .join(character_name)
        .join("user.md");
    if let Ok(content) = std::fs::read_to_string(&char_user) {
        info!(
            character = character_name,
            "Using character-specific user definition"
        );
        return Some(content);
    }

    // 2. Global user.md
    let global_user = config_dir.join("user.md");
    if let Ok(content) = std::fs::read_to_string(&global_user) {
        info!("Using global user definition");
        return Some(content);
    }

    None
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
    fn load_example_config() {
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
[daemon]
socket_path = "/tmp/shore.sock"

[character]
name = "Shore"

[behavior.autonomy]
enabled = true
personality = 0.7
max_unanswered = 2

[advanced]
cache_invalidation_warnings = false
"#,
            ),
            (
                "models.toml",
                r#"
[[models]]
name = "claude-sonnet"
provider = "anthropic"
model_id = "claude-sonnet-4-20250514"

[[models]]
name = "gpt-4o"
provider = "openai"
model_id = "gpt-4o"
"#,
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();

        assert_eq!(
            loaded.app.daemon.socket_path.as_deref(),
            Some("/tmp/shore.sock")
        );
        assert_eq!(loaded.app.character.name, "Shore");
        assert!(loaded.app.behavior.autonomy.enabled);
        assert_eq!(loaded.app.behavior.autonomy.personality, 0.7);
        assert_eq!(loaded.app.behavior.autonomy.max_unanswered, 2);
        assert!(!loaded.app.advanced.cache_invalidation_warnings);

        assert_eq!(loaded.models.models.len(), 2);
        assert!(loaded.models.find_model("claude-sonnet").is_some());
        assert!(loaded.models.find_model("gpt-4o").is_some());
    }

    #[test]
    fn missing_optional_fields_get_defaults() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[character]
name = "TestChar"
"#,
        )]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();

        // All defaults should be filled in.
        assert!(loaded.app.defaults.stream);
        assert!(!loaded.app.behavior.autonomy.enabled);
        assert_eq!(loaded.app.behavior.autonomy.personality, 0.5);
        assert_eq!(loaded.app.behavior.autonomy.max_unanswered, 1);
        assert_eq!(loaded.app.behavior.autonomy.max_deferral_hours, 24.0);
        assert!(loaded.app.behavior.tool_use.enabled);
        assert_eq!(loaded.app.memory.rag_results, 5);
        assert!(loaded.app.advanced.cache_invalidation_warnings);
        assert_eq!(
            loaded.app.behavior.autonomy.heartbeat.session_gap_secs,
            1800
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
    fn invalid_personality_range() {
        let tmp = setup_config_dir(&[(
            "config.toml",
            r#"
[behavior.autonomy]
personality = 1.5
"#,
        )]);

        let config_path = tmp.path().join("config.toml");
        let err = load_config(Some(&config_path)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("personality"), "Should mention personality: {msg}");
    }

    #[test]
    fn invalid_default_model_reference() {
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
[defaults]
model = "nonexistent-model"
"#,
            ),
            ("models.toml", ""),
        ]);

        let config_path = tmp.path().join("config.toml");
        let err = load_config(Some(&config_path)).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nonexistent-model"),
            "Should mention the missing model: {msg}"
        );
    }

    #[test]
    fn duplicate_model_names_rejected() {
        let tmp = setup_config_dir(&[
            ("config.toml", ""),
            (
                "models.toml",
                r#"
[[models]]
name = "dupe"
provider = "anthropic"
model_id = "a"

[[models]]
name = "dupe"
provider = "openai"
model_id = "b"
"#,
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let err = load_config(Some(&config_path)).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("duplicate"), "Should mention duplicate: {msg}");
    }

    #[test]
    fn no_config_files_uses_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        assert_eq!(loaded.app, AppConfig::default());
        assert!(loaded.models.models.is_empty());
    }

    #[test]
    fn character_definition_loaded() {
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
[character]
name = "TestChar"
"#,
            ),
            (
                "characters/TestChar/character.md",
                "You are TestChar, a helpful assistant.",
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        assert_eq!(
            loaded.character_definition.as_deref(),
            Some("You are TestChar, a helpful assistant.")
        );
    }

    #[test]
    fn user_definition_character_specific_overrides_global() {
        let tmp = setup_config_dir(&[
            (
                "config.toml",
                r#"
[character]
name = "TestChar"
"#,
            ),
            ("user.md", "Global user definition"),
            (
                "characters/TestChar/user.md",
                "Character-specific user definition",
            ),
        ]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        assert_eq!(
            loaded.user_definition.as_deref(),
            Some("Character-specific user definition")
        );
    }

    #[test]
    fn user_definition_falls_back_to_global() {
        let tmp = setup_config_dir(&[
            ("config.toml", "[character]\nname = \"TestChar\""),
            ("user.md", "Global user definition"),
        ]);

        let config_path = tmp.path().join("config.toml");
        let loaded = load_config(Some(&config_path)).unwrap();
        assert_eq!(
            loaded.user_definition.as_deref(),
            Some("Global user definition")
        );
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
        let result =
            resolve_prompt_template(tmp.path(), "TestChar", "system.md");
        assert_eq!(result.as_deref(), Some("Character system prompt"));

        // Falls back to global.
        let result =
            resolve_prompt_template(tmp.path(), "TestChar", "compact.md");
        assert_eq!(result.as_deref(), Some("Global compact prompt"));

        // Returns None if no file exists.
        let result =
            resolve_prompt_template(tmp.path(), "TestChar", "nonexistent.md");
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
}
