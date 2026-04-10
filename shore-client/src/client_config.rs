use std::path::PathBuf;

use serde::Deserialize;

/// Client-side configuration loaded from `$XDG_CONFIG_HOME/shore/client.toml`.
///
/// Intentionally separate from the daemon's `config.toml` — clients may run
/// on a different machine and will eventually be packaged independently.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    /// Default server address (`host:port`).
    /// Used when no `--addr` flag is provided.
    pub default_address: Option<String>,
}

/// Return the default path to `client.toml`.
pub fn client_config_path() -> PathBuf {
    shore_config::config_dir().join("client.toml")
}

/// Load client config from the standard path.
///
/// Returns `None` if the file does not exist. Logs a warning and returns
/// `None` if the file exists but cannot be parsed.
pub fn load_client_config() -> Option<ClientConfig> {
    let path = client_config_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("shore: warning: cannot read {}: {e}", path.display());
            return None;
        }
    };
    match toml::from_str::<ClientConfig>(&content) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            eprintln!("shore: warning: invalid {}: {e}", path.display());
            None
        }
    }
}
