//! V1 → V2 data migration compatibility.
//!
//! This module lets the V2 daemon read V1 data files seamlessly:
//! - JSONL conversation files
//! - Conversation manifests (missing `private` defaults to `false`)
//! - `config.toml` with clear errors for deprecated/renamed sections
//! - `models.toml` format
//! - Character data directory validation
//! - Reindex bridge: V1 SQLite → V2 LanceDB

use serde::{Deserialize, Serialize};
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum CompatError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("{}", .0.join("\n"))]
    Config(Vec<String>),
}

// ---------------------------------------------------------------------------
// V1 Conversation JSONL
// ---------------------------------------------------------------------------

/// A single message as stored in V1 JSONL conversation files.
///
/// V1 stored one JSON object per line with `role`, `content`, and `timestamp`.
/// V2 adds optional fields (`images`, `alt_index`, etc.) — this reader
/// accepts the minimal V1 shape and fills defaults for missing fields.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct V1Message {
    pub role: String,
    pub content: String,
    pub timestamp: String,
    /// V1 files may omit msg_id; we generate one if absent.
    #[serde(default)]
    pub msg_id: String,
}

/// Read a V1 JSONL conversation file into a list of messages.
///
/// Each line in the file is a JSON object representing one message.
/// Blank lines are skipped.
pub fn read_v1_jsonl(path: &Path) -> Result<Vec<V1Message>, CompatError> {
    let file = std::fs::File::open(path)?;
    let reader = io::BufReader::new(file);
    let mut messages = Vec::new();

    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut msg: V1Message = serde_json::from_str(trimmed)?;
        // Generate msg_id if V1 file omitted it.
        if msg.msg_id.is_empty() {
            msg.msg_id = format!("v1_imported_{idx}");
        }
        messages.push(msg);
    }

    Ok(messages)
}

// ---------------------------------------------------------------------------
// V1 Conversation Manifest
// ---------------------------------------------------------------------------

/// V1 conversation manifest.
///
/// V1 manifests stored `id` and `title` but did **not** include `private`.
/// The `#[serde(default)]` attribute ensures missing `private` fields
/// deserialize as `false`, matching expected V1 behaviour.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct V1ConversationManifest {
    pub id: String,
    pub title: String,
    /// Defaults to `false` when absent (V1 did not have this field).
    #[serde(default)]
    pub private: bool,
    /// Optional creation timestamp (V1 may or may not include it).
    #[serde(default)]
    pub created_at: Option<String>,
}

/// Read a V1 conversation manifest from a JSON file.
pub fn read_v1_manifest(path: &Path) -> Result<V1ConversationManifest, CompatError> {
    let data = std::fs::read_to_string(path)?;
    let manifest: V1ConversationManifest = serde_json::from_str(&data)?;
    Ok(manifest)
}

/// Read a directory of V1 conversation manifests.
/// Each `.json` file in the directory is treated as a manifest.
pub fn read_v1_manifests(dir: &Path) -> Result<Vec<V1ConversationManifest>, CompatError> {
    let mut manifests = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            manifests.push(read_v1_manifest(&path)?);
        }
    }
    Ok(manifests)
}

// ---------------------------------------------------------------------------
// V1 config.toml
// ---------------------------------------------------------------------------

/// Mapping of deprecated V1 section names to their V2 replacements.
const DEPRECATED_SECTIONS: &[(&str, &str)] = &[
    ("server", "daemon"),
    ("char", "character"),
    ("llm", "models.toml (separate file)"),
];

/// V2 configuration structure.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Config {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub character: CharacterConfig,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DaemonConfig {
    #[serde(default = "default_socket_path")]
    pub socket_path: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
        }
    }
}

fn default_socket_path() -> String {
    "/tmp/shore.sock".to_string()
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CharacterConfig {
    #[serde(default = "default_character_name")]
    pub name: String,
}

impl Default for CharacterConfig {
    fn default() -> Self {
        Self {
            name: default_character_name(),
        }
    }
}

fn default_character_name() -> String {
    "Shore".to_string()
}

/// Result of parsing a V1 config file.
#[derive(Debug)]
pub struct ConfigParseResult {
    pub config: Config,
    /// Warnings about deprecated or renamed sections found in the file.
    pub warnings: Vec<String>,
}

/// Parse a config.toml file, handling V1 compatibility.
///
/// If the file contains deprecated section names (e.g. `[server]` instead of
/// `[daemon]`), the values are migrated and clear warning messages are
/// produced explaining what was renamed.
pub fn parse_config(path: &Path) -> Result<ConfigParseResult, CompatError> {
    let raw = std::fs::read_to_string(path)?;
    parse_config_str(&raw)
}

/// Parse config from a string (testable without filesystem).
pub fn parse_config_str(raw: &str) -> Result<ConfigParseResult, CompatError> {
    let table: toml::Value = toml::from_str(raw)?;
    let mut warnings = Vec::new();

    // Check for deprecated top-level sections.
    if let Some(map) = table.as_table() {
        for &(old_name, new_name) in DEPRECATED_SECTIONS {
            if map.contains_key(old_name) {
                warnings.push(format!(
                    "config.toml: section [{old_name}] is deprecated and has been \
                     renamed to [{new_name}]. Please update your configuration file."
                ));
            }
        }
    }

    // Build a normalized table: remap deprecated names to V2 names.
    let mut normalized = toml::map::Map::new();
    if let Some(map) = table.as_table() {
        for (key, value) in map {
            match key.as_str() {
                "server" => {
                    // Merge into [daemon], V2 values take precedence.
                    merge_into(&mut normalized, "daemon", value);
                }
                "char" => {
                    merge_into(&mut normalized, "character", value);
                }
                "llm" => {
                    // [llm] is fully removed — already warned above.
                }
                _ => {
                    normalized.insert(key.clone(), value.clone());
                }
            }
        }
    }

    let config: Config = toml::Value::Table(normalized)
        .try_into()
        .map_err(|e: toml::de::Error| CompatError::Toml(e))?;

    Ok(ConfigParseResult { config, warnings })
}

/// Merge a TOML value into a target section, creating it if absent.
fn merge_into(target: &mut toml::map::Map<String, toml::Value>, key: &str, value: &toml::Value) {
    let existing = target
        .entry(key.to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));

    if let (Some(dst), Some(src)) = (existing.as_table_mut(), value.as_table()) {
        for (k, v) in src {
            // Don't overwrite V2 values with V1 values.
            dst.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
}

// ---------------------------------------------------------------------------
// V1 models.toml
// ---------------------------------------------------------------------------

/// A single model entry in V2 format.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ModelEntry {
    pub name: String,
    pub provider: String,
    /// V2 field name. V1 files may use `model` instead of `model_id`.
    pub model_id: String,
}

/// Raw V1 model entry that accepts both `model_id` and the legacy `model` field.
#[derive(Deserialize, Debug)]
struct RawModelEntry {
    name: String,
    provider: String,
    /// V2 field.
    model_id: Option<String>,
    /// V1 field (renamed to `model_id` in V2).
    model: Option<String>,
}

/// Container for models.toml.
#[derive(Deserialize, Debug)]
struct RawModelsFile {
    models: Vec<RawModelEntry>,
}

/// Result of parsing a models.toml file.
#[derive(Debug)]
pub struct ModelsParseResult {
    pub models: Vec<ModelEntry>,
    pub warnings: Vec<String>,
}

/// Parse a models.toml file, handling V1 compatibility.
///
/// V1 used `model` instead of `model_id`. If the legacy field is found,
/// it is transparently mapped and a warning is emitted.
pub fn parse_models(path: &Path) -> Result<ModelsParseResult, CompatError> {
    let raw = std::fs::read_to_string(path)?;
    parse_models_str(&raw)
}

/// Parse models from a string (testable without filesystem).
pub fn parse_models_str(raw: &str) -> Result<ModelsParseResult, CompatError> {
    let file: RawModelsFile = toml::from_str(raw)?;
    let mut warnings = Vec::new();
    let mut models = Vec::new();

    for entry in file.models {
        let model_id = match (entry.model_id, entry.model) {
            (Some(id), _) => id,
            (None, Some(legacy)) => {
                warnings.push(format!(
                    "models.toml: model '{}' uses deprecated field `model` — \
                     please rename to `model_id`.",
                    entry.name
                ));
                legacy
            }
            (None, None) => {
                return Err(CompatError::Config(vec![format!(
                    "models.toml: model '{}' is missing required field `model_id`.",
                    entry.name
                )]));
            }
        };

        models.push(ModelEntry {
            name: entry.name,
            provider: entry.provider,
            model_id,
        });
    }

    Ok(ModelsParseResult { models, warnings })
}

// ---------------------------------------------------------------------------
// Character data directory validation
// ---------------------------------------------------------------------------

/// Expected subdirectories/files in a character data directory.
#[derive(Debug)]
pub struct CharacterDirStatus {
    /// Root directory exists.
    pub root_exists: bool,
    /// `memory/` subdirectory exists.
    pub memory_dir: bool,
    /// `memory/memory.db` exists.
    pub memory_db: bool,
    /// `memory/vectorstore/` exists.
    pub vectorstore_dir: bool,
    /// Conversation directories found (V1 stored conversations as subdirs).
    pub conversation_dirs: Vec<PathBuf>,
    /// Any unexpected files or directories.
    pub other_entries: Vec<PathBuf>,
}

/// Validate a V1/V2 character data directory.
///
/// Returns a status struct describing which expected components are present.
/// This allows the caller to decide what needs initialization vs. migration.
pub fn validate_character_dir(character_dir: &Path) -> CharacterDirStatus {
    let root_exists = character_dir.is_dir();
    let memory_dir_path = character_dir.join("memory");
    let memory_dir = memory_dir_path.is_dir();
    let memory_db = memory_dir_path.join("memory.db").is_file();
    let vectorstore_dir = memory_dir_path.join("vectorstore").is_dir();

    let mut conversation_dirs = Vec::new();
    let mut other_entries = Vec::new();

    let conversations_path = character_dir.join("conversations");
    if conversations_path.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&conversations_path) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    conversation_dirs.push(path);
                }
            }
        }
    }

    // Scan root for unexpected entries.
    if root_exists {
        if let Ok(entries) = std::fs::read_dir(character_dir) {
            let known = ["memory", "conversations", "matrix"];
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if !known.contains(&name_str.as_ref()) {
                    other_entries.push(entry.path());
                }
            }
        }
    }

    CharacterDirStatus {
        root_exists,
        memory_dir,
        memory_db,
        vectorstore_dir,
        conversation_dirs,
        other_entries,
    }
}

// ---------------------------------------------------------------------------
// Reindex bridge: V1 SQLite entries → V2 LanceDB
// ---------------------------------------------------------------------------

/// Entry data needed for reindexing (minimal projection from SQLite).
#[derive(Debug, Clone)]
pub struct ReindexEntry {
    pub entry_id: String,
    pub summary_text: String,
}

/// Extract all active entries from a V1/V2 SQLite database for reindexing.
///
/// This reads entries with status "active" or "protected" and returns their
/// IDs and summary text so they can be embedded and inserted into LanceDB.
pub fn extract_entries_for_reindex(
    db: &crate::memory::db::MemoryDB,
) -> Result<Vec<ReindexEntry>, rusqlite::Error> {
    let mut entries = Vec::new();
    for status in &["active", "protected"] {
        for entry in db.get_entries_by_status(status)? {
            if !entry.summary_text.is_empty() {
                entries.push(ReindexEntry {
                    entry_id: entry.id,
                    summary_text: entry.summary_text,
                });
            }
        }
    }
    Ok(entries)
}

/// Collect all active+protected entries from the DB, suitable for passing
/// to `VectorStore::reindex()` once embeddings have been computed.
///
/// This is a synchronous extraction step. The caller is responsible for
/// computing embeddings (via `embed_text`) and calling `VectorStore::reindex`.
///
/// Example workflow:
/// ```ignore
/// let db = MemoryDB::open_v1(&path)?;
/// let entries = extract_entries_for_reindex(&db)?;
/// // compute embeddings externally...
/// // vectorstore.reindex(&pairs).await?;
/// ```
pub fn entries_to_reindex_pairs<'a>(
    entries: &'a [ReindexEntry],
    embeddings: &'a [Vec<f32>],
) -> Vec<(&'a str, &'a [f32])> {
    entries
        .iter()
        .zip(embeddings.iter())
        .map(|(e, emb)| (e.entry_id.as_str(), emb.as_slice()))
        .collect()
}

// ---------------------------------------------------------------------------
// Full V1 conversation directory reader
// ---------------------------------------------------------------------------

/// A complete V1 conversation: manifest + messages.
#[derive(Debug, Clone)]
pub struct V1Conversation {
    pub manifest: V1ConversationManifest,
    pub messages: Vec<V1Message>,
}

/// Read a V1 conversation directory.
///
/// V1 stored each conversation as a directory containing:
/// - `manifest.json` — conversation metadata
/// - `messages.jsonl` — message history
pub fn read_v1_conversation(conv_dir: &Path) -> Result<V1Conversation, CompatError> {
    let manifest_path = conv_dir.join("manifest.json");
    let messages_path = conv_dir.join("messages.jsonl");

    let manifest = read_v1_manifest(&manifest_path)?;
    let messages = if messages_path.exists() {
        read_v1_jsonl(&messages_path)?
    } else {
        Vec::new()
    };

    Ok(V1Conversation { manifest, messages })
}

/// Read all V1 conversations from a character's conversations directory.
pub fn read_all_v1_conversations(
    conversations_dir: &Path,
) -> Result<Vec<V1Conversation>, CompatError> {
    let mut conversations = Vec::new();
    if !conversations_dir.is_dir() {
        return Ok(conversations);
    }
    for entry in std::fs::read_dir(conversations_dir)? {
        let entry = entry?;
        if entry.path().is_dir() {
            conversations.push(read_v1_conversation(&entry.path())?);
        }
    }
    Ok(conversations)
}

// ---------------------------------------------------------------------------
// V1 data directory summary
// ---------------------------------------------------------------------------

/// Full validation result for a V1 character data directory.
#[derive(Debug)]
pub struct V1ValidationResult {
    pub dir_status: CharacterDirStatus,
    pub config_result: Option<ConfigParseResult>,
    pub models_result: Option<ModelsParseResult>,
    pub conversation_count: usize,
    pub all_warnings: Vec<String>,
}

/// Perform a complete validation of a V1 data directory.
///
/// This checks directory structure, parses config and model files if present,
/// and counts conversations. All warnings are collected into `all_warnings`.
pub fn validate_v1_data(
    character_dir: &Path,
    config_path: Option<&Path>,
    models_path: Option<&Path>,
) -> V1ValidationResult {
    let dir_status = validate_character_dir(character_dir);
    let mut all_warnings = Vec::new();

    let config_result = config_path.and_then(|p| match parse_config(p) {
        Ok(result) => {
            all_warnings.extend(result.warnings.clone());
            Some(result)
        }
        Err(e) => {
            all_warnings.push(format!("Failed to parse config.toml: {e}"));
            None
        }
    });

    let models_result = models_path.and_then(|p| match parse_models(p) {
        Ok(result) => {
            all_warnings.extend(result.warnings.clone());
            Some(result)
        }
        Err(e) => {
            all_warnings.push(format!("Failed to parse models.toml: {e}"));
            None
        }
    });

    let conversations_dir = character_dir.join("conversations");
    let conversation_count = if conversations_dir.is_dir() {
        std::fs::read_dir(&conversations_dir)
            .map(|entries| entries.filter_map(Result::ok).filter(|e| e.path().is_dir()).count())
            .unwrap_or(0)
    } else {
        0
    };

    V1ValidationResult {
        dir_status,
        config_result,
        models_result,
        conversation_count,
        all_warnings,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -- JSONL conversations -----------------------------------------------

    #[test]
    fn test_read_v1_jsonl() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("messages.jsonl");
        std::fs::write(
            &path,
            r#"{"role":"user","content":"Hello","timestamp":"2024-06-01T10:00:00Z"}
{"role":"assistant","content":"Hi there!","timestamp":"2024-06-01T10:00:01Z"}
"#,
        )
        .unwrap();

        let messages = read_v1_jsonl(&path).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Hello");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "Hi there!");
        // msg_id should be auto-generated for V1 files.
        assert_eq!(messages[0].msg_id, "v1_imported_0");
        assert_eq!(messages[1].msg_id, "v1_imported_1");
    }

    #[test]
    fn test_read_v1_jsonl_with_msg_id() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("messages.jsonl");
        std::fs::write(
            &path,
            r#"{"role":"user","content":"Hello","timestamp":"2024-06-01T10:00:00Z","msg_id":"abc123"}
"#,
        )
        .unwrap();

        let messages = read_v1_jsonl(&path).unwrap();
        assert_eq!(messages[0].msg_id, "abc123");
    }

    #[test]
    fn test_read_v1_jsonl_blank_lines() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("messages.jsonl");
        std::fs::write(
            &path,
            r#"{"role":"user","content":"A","timestamp":"2024-06-01T10:00:00Z"}

{"role":"user","content":"B","timestamp":"2024-06-01T10:00:01Z"}
"#,
        )
        .unwrap();

        let messages = read_v1_jsonl(&path).unwrap();
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_read_v1_jsonl_empty_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("messages.jsonl");
        std::fs::write(&path, "").unwrap();

        let messages = read_v1_jsonl(&path).unwrap();
        assert!(messages.is_empty());
    }

    // -- Conversation manifests --------------------------------------------

    #[test]
    fn test_read_v1_manifest_without_private() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("manifest.json");
        std::fs::write(
            &path,
            r#"{"id":"conv-001","title":"First chat"}"#,
        )
        .unwrap();

        let manifest = read_v1_manifest(&path).unwrap();
        assert_eq!(manifest.id, "conv-001");
        assert_eq!(manifest.title, "First chat");
        assert!(!manifest.private, "V1 manifest missing private should default to false");
        assert!(manifest.created_at.is_none());
    }

    #[test]
    fn test_read_v1_manifest_with_private() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("manifest.json");
        std::fs::write(
            &path,
            r#"{"id":"conv-002","title":"Secret chat","private":true}"#,
        )
        .unwrap();

        let manifest = read_v1_manifest(&path).unwrap();
        assert!(manifest.private);
    }

    #[test]
    fn test_read_v1_manifest_with_created_at() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("manifest.json");
        std::fs::write(
            &path,
            r#"{"id":"conv-003","title":"Dated","created_at":"2024-06-01T10:00:00Z"}"#,
        )
        .unwrap();

        let manifest = read_v1_manifest(&path).unwrap();
        assert_eq!(manifest.created_at.as_deref(), Some("2024-06-01T10:00:00Z"));
        assert!(!manifest.private);
    }

    #[test]
    fn test_read_v1_manifests_dir() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("manifests");
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("a.json"),
            r#"{"id":"a","title":"Chat A"}"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("b.json"),
            r#"{"id":"b","title":"Chat B"}"#,
        )
        .unwrap();
        // Non-json file should be skipped.
        std::fs::write(dir.join("readme.txt"), "ignore me").unwrap();

        let manifests = read_v1_manifests(&dir).unwrap();
        assert_eq!(manifests.len(), 2);
    }

    // -- Config parsing ----------------------------------------------------

    #[test]
    fn test_parse_v2_config() {
        let raw = r#"
[daemon]
socket_path = "/tmp/shore.sock"

[character]
name = "Shore"
"#;
        let result = parse_config_str(raw).unwrap();
        assert_eq!(result.config.daemon.socket_path, "/tmp/shore.sock");
        assert_eq!(result.config.character.name, "Shore");
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_parse_v1_config_deprecated_server() {
        let raw = r#"
[server]
socket_path = "/tmp/shore-v1.sock"

[character]
name = "ShoreV1"
"#;
        let result = parse_config_str(raw).unwrap();
        assert_eq!(result.config.daemon.socket_path, "/tmp/shore-v1.sock");
        assert_eq!(result.config.character.name, "ShoreV1");
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("[server]"));
        assert!(result.warnings[0].contains("[daemon]"));
    }

    #[test]
    fn test_parse_v1_config_deprecated_char() {
        let raw = r#"
[daemon]
socket_path = "/tmp/shore.sock"

[char]
name = "OldChar"
"#;
        let result = parse_config_str(raw).unwrap();
        assert_eq!(result.config.character.name, "OldChar");
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("[char]"));
        assert!(result.warnings[0].contains("[character]"));
    }

    #[test]
    fn test_parse_v1_config_deprecated_llm() {
        let raw = r#"
[daemon]
socket_path = "/tmp/shore.sock"

[llm]
default_model = "gpt-4"
"#;
        let result = parse_config_str(raw).unwrap();
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("[llm]"));
        assert!(result.warnings[0].contains("models.toml"));
    }

    #[test]
    fn test_parse_v1_config_all_deprecated() {
        let raw = r#"
[server]
socket_path = "/tmp/shore-v1.sock"

[char]
name = "OldChar"

[llm]
default_model = "gpt-4"
"#;
        let result = parse_config_str(raw).unwrap();
        assert_eq!(result.warnings.len(), 3);
    }

    #[test]
    fn test_parse_config_v2_takes_precedence() {
        let raw = r#"
[daemon]
socket_path = "/tmp/v2.sock"

[server]
socket_path = "/tmp/v1.sock"
"#;
        let result = parse_config_str(raw).unwrap();
        // V2 [daemon] should take precedence over deprecated [server].
        assert_eq!(result.config.daemon.socket_path, "/tmp/v2.sock");
        assert_eq!(result.warnings.len(), 1);
    }

    #[test]
    fn test_parse_config_defaults() {
        let raw = "";
        let result = parse_config_str(raw).unwrap();
        assert_eq!(result.config.daemon.socket_path, "/tmp/shore.sock");
        assert_eq!(result.config.character.name, "Shore");
    }

    // -- Models parsing ----------------------------------------------------

    #[test]
    fn test_parse_v2_models() {
        let raw = r#"
[[models]]
name = "claude-sonnet"
provider = "anthropic"
model_id = "claude-sonnet-4-20250514"

[[models]]
name = "gpt-4o"
provider = "openai"
model_id = "gpt-4o"
"#;
        let result = parse_models_str(raw).unwrap();
        assert_eq!(result.models.len(), 2);
        assert_eq!(result.models[0].name, "claude-sonnet");
        assert_eq!(result.models[0].model_id, "claude-sonnet-4-20250514");
        assert_eq!(result.models[1].name, "gpt-4o");
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_parse_v1_models_legacy_field() {
        let raw = r#"
[[models]]
name = "claude-sonnet"
provider = "anthropic"
model = "claude-sonnet-4-20250514"
"#;
        let result = parse_models_str(raw).unwrap();
        assert_eq!(result.models.len(), 1);
        assert_eq!(result.models[0].model_id, "claude-sonnet-4-20250514");
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("deprecated field `model`"));
        assert!(result.warnings[0].contains("model_id"));
    }

    #[test]
    fn test_parse_v1_models_model_id_takes_precedence() {
        let raw = r#"
[[models]]
name = "test"
provider = "anthropic"
model_id = "new-id"
model = "old-id"
"#;
        let result = parse_models_str(raw).unwrap();
        assert_eq!(result.models[0].model_id, "new-id");
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn test_parse_models_missing_model_id() {
        let raw = r#"
[[models]]
name = "broken"
provider = "anthropic"
"#;
        let result = parse_models_str(raw);
        assert!(result.is_err());
        if let Err(CompatError::Config(errors)) = result {
            assert!(errors[0].contains("missing required field `model_id`"));
        }
    }

    // -- Character directory validation ------------------------------------

    #[test]
    fn test_validate_empty_dir() {
        let tmp = TempDir::new().unwrap();
        let status = validate_character_dir(tmp.path());
        assert!(status.root_exists);
        assert!(!status.memory_dir);
        assert!(!status.memory_db);
        assert!(!status.vectorstore_dir);
        assert!(status.conversation_dirs.is_empty());
    }

    #[test]
    fn test_validate_nonexistent_dir() {
        let status = validate_character_dir(Path::new("/nonexistent/path"));
        assert!(!status.root_exists);
    }

    #[test]
    fn test_validate_v1_dir_structure() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create V1 structure.
        std::fs::create_dir_all(root.join("memory")).unwrap();
        std::fs::write(root.join("memory/memory.db"), b"sqlite").unwrap();
        std::fs::create_dir_all(root.join("conversations/conv-001")).unwrap();
        std::fs::create_dir_all(root.join("conversations/conv-002")).unwrap();

        let status = validate_character_dir(root);
        assert!(status.root_exists);
        assert!(status.memory_dir);
        assert!(status.memory_db);
        assert!(!status.vectorstore_dir);
        assert_eq!(status.conversation_dirs.len(), 2);
    }

    #[test]
    fn test_validate_v2_dir_structure() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("memory/vectorstore")).unwrap();
        std::fs::write(root.join("memory/memory.db"), b"sqlite").unwrap();
        std::fs::create_dir_all(root.join("matrix")).unwrap();

        let status = validate_character_dir(root);
        assert!(status.root_exists);
        assert!(status.memory_dir);
        assert!(status.memory_db);
        assert!(status.vectorstore_dir);
        assert!(status.other_entries.is_empty());
    }

    #[test]
    fn test_validate_dir_with_unknown_entries() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        std::fs::create_dir_all(root.join("memory")).unwrap();
        std::fs::create_dir_all(root.join("unknown_dir")).unwrap();
        std::fs::write(root.join("stray_file.txt"), b"oops").unwrap();

        let status = validate_character_dir(root);
        assert_eq!(status.other_entries.len(), 2);
    }

    // -- Full conversation reading -----------------------------------------

    #[test]
    fn test_read_v1_conversation() {
        let tmp = TempDir::new().unwrap();
        let conv_dir = tmp.path().join("conv-001");
        std::fs::create_dir_all(&conv_dir).unwrap();

        std::fs::write(
            conv_dir.join("manifest.json"),
            r#"{"id":"conv-001","title":"Test Chat"}"#,
        )
        .unwrap();
        std::fs::write(
            conv_dir.join("messages.jsonl"),
            r#"{"role":"user","content":"Hi","timestamp":"2024-06-01T10:00:00Z"}
{"role":"assistant","content":"Hello!","timestamp":"2024-06-01T10:00:01Z"}
"#,
        )
        .unwrap();

        let conv = read_v1_conversation(&conv_dir).unwrap();
        assert_eq!(conv.manifest.id, "conv-001");
        assert!(!conv.manifest.private);
        assert_eq!(conv.messages.len(), 2);
    }

    #[test]
    fn test_read_v1_conversation_no_messages() {
        let tmp = TempDir::new().unwrap();
        let conv_dir = tmp.path().join("conv-002");
        std::fs::create_dir_all(&conv_dir).unwrap();

        std::fs::write(
            conv_dir.join("manifest.json"),
            r#"{"id":"conv-002","title":"Empty Chat"}"#,
        )
        .unwrap();

        let conv = read_v1_conversation(&conv_dir).unwrap();
        assert_eq!(conv.manifest.id, "conv-002");
        assert!(conv.messages.is_empty());
    }

    #[test]
    fn test_read_all_v1_conversations() {
        let tmp = TempDir::new().unwrap();
        let convs_dir = tmp.path().join("conversations");

        for i in 0..3 {
            let dir = convs_dir.join(format!("conv-{i:03}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("manifest.json"),
                format!(r#"{{"id":"conv-{i:03}","title":"Chat {i}"}}"#),
            )
            .unwrap();
            std::fs::write(
                dir.join("messages.jsonl"),
                format!(
                    r#"{{"role":"user","content":"Msg {i}","timestamp":"2024-06-01T10:00:0{i}Z"}}"#
                ) + "\n",
            )
            .unwrap();
        }

        let conversations = read_all_v1_conversations(&convs_dir).unwrap();
        assert_eq!(conversations.len(), 3);
    }

    #[test]
    fn test_read_all_v1_conversations_nonexistent_dir() {
        let conversations = read_all_v1_conversations(Path::new("/nonexistent")).unwrap();
        assert!(conversations.is_empty());
    }

    // -- Reindex bridge ----------------------------------------------------

    #[test]
    fn test_extract_entries_for_reindex() {
        use crate::memory::db::MemoryDB;

        let db = MemoryDB::open_in_memory().unwrap();

        // Insert entries with different statuses.
        let now = chrono::Utc::now().to_rfc3339();
        let make = |id: &str, status: &str, text: &str| crate::memory::db::Entry {
            id: id.to_string(),
            memory_type: "episodic".to_string(),
            source: "summary".to_string(),
            reason: "compaction".to_string(),
            status: status.to_string(),
            confidence: 0.9,
            summary_text: text.to_string(),
            topic_tags: String::new(),
            topic_key: String::new(),
            start_timestamp: now.clone(),
            end_timestamp: now.clone(),
            message_count: 1,
            source_entry_ids: String::new(),
            related_entry_ids: String::new(),
            superseded_by: String::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
            entry_type: String::new(),
            image_path: String::new(),
            collated_at: String::new(),
        };

        db.create_entry(&make("e1", "active", "Active entry")).unwrap();
        db.create_entry(&make("e2", "protected", "Protected entry")).unwrap();
        db.create_entry(&make("e3", "superseded", "Old entry")).unwrap();
        db.create_entry(&make("e4", "active", "")).unwrap(); // empty text

        let entries = extract_entries_for_reindex(&db).unwrap();
        // Only active and protected with non-empty text.
        assert_eq!(entries.len(), 2);
        let ids: Vec<&str> = entries.iter().map(|e| e.entry_id.as_str()).collect();
        assert!(ids.contains(&"e1"));
        assert!(ids.contains(&"e2"));
    }

    #[test]
    fn test_entries_to_reindex_pairs() {
        let entries = vec![
            ReindexEntry {
                entry_id: "e1".to_string(),
                summary_text: "Hello".to_string(),
            },
            ReindexEntry {
                entry_id: "e2".to_string(),
                summary_text: "World".to_string(),
            },
        ];
        let embeddings = vec![vec![1.0, 0.0, 0.0], vec![0.0, 1.0, 0.0]];

        let pairs = entries_to_reindex_pairs(&entries, &embeddings);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].0, "e1");
        assert_eq!(pairs[0].1, &[1.0, 0.0, 0.0]);
        assert_eq!(pairs[1].0, "e2");
        assert_eq!(pairs[1].1, &[0.0, 1.0, 0.0]);
    }

    // -- V1 full validation ------------------------------------------------

    #[test]
    fn test_validate_v1_data_full() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        // Create character dir with conversations.
        std::fs::create_dir_all(root.join("memory")).unwrap();
        std::fs::write(root.join("memory/memory.db"), b"sqlite").unwrap();
        let conv_dir = root.join("conversations/conv-001");
        std::fs::create_dir_all(&conv_dir).unwrap();
        std::fs::write(
            conv_dir.join("manifest.json"),
            r#"{"id":"conv-001","title":"Chat"}"#,
        )
        .unwrap();

        // Create V1 config with deprecated sections.
        let config_path = tmp.path().join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[server]
socket_path = "/tmp/shore.sock"

[char]
name = "TestChar"
"#,
        )
        .unwrap();

        // Create V1 models with legacy field.
        let models_path = tmp.path().join("models.toml");
        std::fs::write(
            &models_path,
            r#"
[[models]]
name = "test-model"
provider = "openai"
model = "gpt-4"
"#,
        )
        .unwrap();

        let result = validate_v1_data(root, Some(&config_path), Some(&models_path));
        assert!(result.dir_status.root_exists);
        assert!(result.dir_status.memory_db);
        assert_eq!(result.conversation_count, 1);
        // 2 config warnings (server, char) + 1 models warning (model field).
        assert_eq!(result.all_warnings.len(), 3);
        assert!(result.config_result.is_some());
        assert!(result.models_result.is_some());
    }

    // -- V1 SQLite + reindex integration -----------------------------------

    #[test]
    fn test_v1_db_reindex_integration() {
        use crate::memory::db::MemoryDB;

        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("v1.db");

        // Create a V1-style database.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS entries (
                    id TEXT PRIMARY KEY, memory_type TEXT NOT NULL,
                    source TEXT NOT NULL DEFAULT '', reason TEXT NOT NULL DEFAULT '',
                    status TEXT NOT NULL DEFAULT 'active',
                    confidence REAL NOT NULL DEFAULT 1.0, summary_text TEXT NOT NULL DEFAULT '',
                    topic_tags TEXT NOT NULL DEFAULT '', topic_key TEXT NOT NULL DEFAULT '',
                    start_timestamp TEXT NOT NULL DEFAULT '', end_timestamp TEXT NOT NULL DEFAULT '',
                    message_count INTEGER NOT NULL DEFAULT 0,
                    source_entry_ids TEXT NOT NULL DEFAULT '',
                    related_entry_ids TEXT NOT NULL DEFAULT '',
                    superseded_by TEXT NOT NULL DEFAULT '',
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                    entry_type TEXT NOT NULL DEFAULT '', image_path TEXT NOT NULL DEFAULT ''
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO entries (id, memory_type, summary_text, created_at, updated_at)
                 VALUES ('20240601_100000_0', 'episodic', 'V1 memory about Tokyo trip',
                         '2024-06-01T10:00:00Z', '2024-06-01T10:00:00Z')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO entries (id, memory_type, summary_text, status, created_at, updated_at)
                 VALUES ('20240601_110000_0', 'semantic', 'V1 fact about user', 'protected',
                         '2024-06-01T11:00:00Z', '2024-06-01T11:00:00Z')",
                [],
            )
            .unwrap();
        }

        // Open with V2 code.
        let db = MemoryDB::open_v1(&db_path).unwrap();

        // Extract entries for reindex.
        let entries = extract_entries_for_reindex(&db).unwrap();
        assert_eq!(entries.len(), 2);

        // Verify the entries can be paired with embeddings.
        let embeddings = vec![vec![1.0, 0.0, 0.0, 0.0], vec![0.0, 1.0, 0.0, 0.0]];
        let pairs = entries_to_reindex_pairs(&entries, &embeddings);
        assert_eq!(pairs.len(), 2);
    }

    // -- Config file on disk -----------------------------------------------

    #[test]
    fn test_parse_config_from_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[daemon]
socket_path = "/tmp/test.sock"

[character]
name = "TestChar"
"#,
        )
        .unwrap();

        let result = parse_config(&path).unwrap();
        assert_eq!(result.config.daemon.socket_path, "/tmp/test.sock");
        assert_eq!(result.config.character.name, "TestChar");
    }

    #[test]
    fn test_parse_models_from_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("models.toml");
        std::fs::write(
            &path,
            r#"
[[models]]
name = "test"
provider = "openai"
model_id = "gpt-4o"
"#,
        )
        .unwrap();

        let result = parse_models(&path).unwrap();
        assert_eq!(result.models.len(), 1);
        assert_eq!(result.models[0].model_id, "gpt-4o");
    }
}
