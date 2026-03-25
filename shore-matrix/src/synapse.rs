use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};
use tracing::{error, info, warn};

/// Configuration for a managed Synapse instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynapseConfig {
    /// Server name (e.g. "localhost")
    pub server_name: String,
    /// Port for the HTTP listener
    pub port: u16,
    /// Path to the Synapse data directory
    pub data_dir: PathBuf,
    /// Shared secret for registration (generated on first run)
    pub registration_shared_secret: String,
    /// Whether to enable user registration
    pub enable_registration: bool,
    /// Log level for Synapse
    pub log_level: String,
}

impl Default for SynapseConfig {
    fn default() -> Self {
        Self {
            server_name: "localhost".to_string(),
            port: 8008,
            data_dir: PathBuf::from("/tmp/shore-synapse"),
            registration_shared_secret: String::new(),
            log_level: "WARNING".to_string(),
            enable_registration: false,
        }
    }
}

impl SynapseConfig {
    /// Generate a YAML homeserver.yaml configuration for Synapse.
    pub fn generate_yaml(&self) -> String {
        let db_path = self.data_dir.join("homeserver.db");
        let media_store = self.data_dir.join("media_store");
        let signing_key = self.data_dir.join("signing.key");
        let log_config = self.data_dir.join("log.config");

        format!(
            r#"server_name: "{server_name}"
pid_file: "{pid_file}"
listeners:
  - port: {port}
    tls: false
    type: http
    x_forwarded: true
    resources:
      - names: [client, federation]
        compress: false
database:
  name: sqlite3
  args:
    database: "{db_path}"
log_config: "{log_config}"
media_store_path: "{media_store}"
registration_shared_secret: "{secret}"
enable_registration: {enable_reg}
report_stats: false
signing_key_path: "{signing_key}"
suppress_key_server_warning: true
"#,
            server_name = self.server_name,
            pid_file = self.data_dir.join("homeserver.pid").display(),
            port = self.port,
            db_path = db_path.display(),
            log_config = log_config.display(),
            media_store = media_store.display(),
            secret = self.registration_shared_secret,
            enable_reg = self.enable_registration,
            signing_key = signing_key.display(),
        )
    }

    /// Generate the Synapse log.config file content.
    pub fn generate_log_config(&self) -> String {
        let log_file = self.data_dir.join("homeserver.log");
        format!(
            r#"version: 1
formatters:
  precise:
    format: '%(asctime)s - %(name)s - %(lineno)d - %(levelname)s - %(request)s - %(message)s'
handlers:
  file:
    class: logging.handlers.RotatingFileHandler
    formatter: precise
    filename: "{log_file}"
    maxBytes: 10485760
    backupCount: 3
  console:
    class: logging.StreamHandler
    formatter: precise
loggers:
  synapse.storage.SQL:
    level: WARNING
root:
  level: {level}
  handlers: [file, console]
disable_existing_loggers: false
"#,
            log_file = log_file.display(),
            level = self.log_level,
        )
    }
}

/// Manages a Synapse subprocess lifecycle.
pub struct SynapseManager {
    config: SynapseConfig,
    child: Option<Child>,
}

impl SynapseManager {
    pub fn new(config: SynapseConfig) -> Self {
        Self {
            config,
            child: None,
        }
    }

    /// Write config files and start the Synapse process.
    pub async fn start(&mut self) -> Result<(), SynapseError> {
        if self.child.is_some() {
            return Err(SynapseError::AlreadyRunning);
        }

        // Ensure directories exist
        tokio::fs::create_dir_all(&self.config.data_dir)
            .await
            .map_err(|e| SynapseError::Io(format!("create data dir: {e}")))?;
        tokio::fs::create_dir_all(self.config.data_dir.join("media_store"))
            .await
            .map_err(|e| SynapseError::Io(format!("create media store: {e}")))?;

        // Write config files
        let config_path = self.config.data_dir.join("homeserver.yaml");
        let log_config_path = self.config.data_dir.join("log.config");

        tokio::fs::write(&config_path, self.config.generate_yaml())
            .await
            .map_err(|e| SynapseError::Io(format!("write homeserver.yaml: {e}")))?;
        tokio::fs::write(&log_config_path, self.config.generate_log_config())
            .await
            .map_err(|e| SynapseError::Io(format!("write log.config: {e}")))?;

        info!("starting Synapse with config at {}", config_path.display());

        let child = Command::new("python")
            .arg("-m")
            .arg("synapse.app.homeserver")
            .arg("--config-path")
            .arg(&config_path)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| SynapseError::SpawnFailed(e.to_string()))?;

        self.child = Some(child);
        info!("Synapse started (port {})", self.config.port);
        Ok(())
    }

    /// Stop the Synapse process.
    pub async fn stop(&mut self) -> Result<(), SynapseError> {
        if let Some(mut child) = self.child.take() {
            info!("stopping Synapse");
            child
                .kill()
                .await
                .map_err(|e| SynapseError::Io(format!("kill: {e}")))?;
            Ok(())
        } else {
            Err(SynapseError::NotRunning)
        }
    }

    /// Check if the Synapse process is running and the HTTP endpoint is healthy.
    pub async fn health_check(&mut self) -> HealthStatus {
        // Check process is alive
        if let Some(ref mut child) = self.child {
            match child.try_wait() {
                Ok(Some(status)) => {
                    warn!("Synapse exited with status: {status}");
                    self.child = None;
                    return HealthStatus::ProcessExited(status.code());
                }
                Ok(None) => { /* still running */ }
                Err(e) => {
                    error!("failed to check Synapse status: {e}");
                    return HealthStatus::Unknown;
                }
            }
        } else {
            return HealthStatus::NotRunning;
        }

        // Check HTTP health endpoint
        match http_health_check(&self.config.homeserver_url()).await {
            Ok(true) => HealthStatus::Healthy,
            Ok(false) => HealthStatus::Unhealthy,
            Err(_) => HealthStatus::Unhealthy,
        }
    }

    pub fn config(&self) -> &SynapseConfig {
        &self.config
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }
}

impl SynapseConfig {
    /// The homeserver URL for this config.
    pub fn homeserver_url(&self) -> String {
        format!("http://localhost:{}", self.port)
    }
}

/// Check the Synapse `/_matrix/client/versions` endpoint.
async fn http_health_check(homeserver_url: &str) -> Result<bool, String> {
    let url = format!("{homeserver_url}/_matrix/client/versions");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    Ok(resp.status().is_success())
}

/// Poll until Synapse responds to health checks, or timeout.
pub async fn wait_for_healthy(homeserver_url: &str, timeout: Duration) -> bool {
    let start = tokio::time::Instant::now();
    let interval = Duration::from_millis(500);
    loop {
        if start.elapsed() >= timeout {
            return false;
        }
        if let Ok(true) = http_health_check(homeserver_url).await {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Unhealthy,
    NotRunning,
    ProcessExited(Option<i32>),
    Unknown,
}

#[derive(Debug)]
pub enum SynapseError {
    AlreadyRunning,
    NotRunning,
    SpawnFailed(String),
    Io(String),
}

impl std::fmt::Display for SynapseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "Synapse is already running"),
            Self::NotRunning => write!(f, "Synapse is not running"),
            Self::SpawnFailed(e) => write!(f, "failed to spawn Synapse: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for SynapseError {}

/// Generate a random registration shared secret.
pub fn generate_shared_secret() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let s = RandomState::new();
    let mut h = s.build_hasher();
    h.write_u64(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64);
    let v1 = h.finish();
    let mut h2 = s.build_hasher();
    h2.write_u64(v1.wrapping_mul(6364136223846793005));
    let v2 = h2.finish();
    format!("{v1:016x}{v2:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = SynapseConfig::default();
        assert_eq!(config.server_name, "localhost");
        assert_eq!(config.port, 8008);
        assert!(!config.enable_registration);
    }

    #[test]
    fn generate_yaml_contains_required_fields() {
        let config = SynapseConfig {
            server_name: "test.shore.local".to_string(),
            port: 9999,
            data_dir: PathBuf::from("/tmp/test-synapse"),
            registration_shared_secret: "supersecret123".to_string(),
            enable_registration: true,
            log_level: "INFO".to_string(),
        };
        let yaml = config.generate_yaml();
        assert!(yaml.contains("server_name: \"test.shore.local\""));
        assert!(yaml.contains("port: 9999"));
        assert!(yaml.contains("registration_shared_secret: \"supersecret123\""));
        assert!(yaml.contains("enable_registration: true"));
        assert!(yaml.contains("database: \"/tmp/test-synapse/homeserver.db\""));
        assert!(yaml.contains("media_store_path: \"/tmp/test-synapse/media_store\""));
        assert!(yaml.contains("signing_key_path: \"/tmp/test-synapse/signing.key\""));
        assert!(yaml.contains("suppress_key_server_warning: true"));
    }

    #[test]
    fn generate_yaml_registration_disabled() {
        let config = SynapseConfig {
            enable_registration: false,
            ..SynapseConfig::default()
        };
        let yaml = config.generate_yaml();
        assert!(yaml.contains("enable_registration: false"));
    }

    #[test]
    fn generate_log_config_contains_required_fields() {
        let config = SynapseConfig {
            data_dir: PathBuf::from("/tmp/test-synapse"),
            log_level: "DEBUG".to_string(),
            ..SynapseConfig::default()
        };
        let log_config = config.generate_log_config();
        assert!(log_config.contains("level: DEBUG"));
        assert!(log_config.contains("/tmp/test-synapse/homeserver.log"));
        assert!(log_config.contains("RotatingFileHandler"));
    }

    #[test]
    fn homeserver_url() {
        let config = SynapseConfig {
            port: 8448,
            ..SynapseConfig::default()
        };
        assert_eq!(config.homeserver_url(), "http://localhost:8448");
    }

    #[test]
    fn generate_shared_secret_unique() {
        let s1 = generate_shared_secret();
        let s2 = generate_shared_secret();
        assert_eq!(s1.len(), 32);
        assert_eq!(s2.len(), 32);
        // Not strictly guaranteed but extremely unlikely to collide
        assert_ne!(s1, s2);
    }

    #[test]
    fn health_status_variants() {
        assert_eq!(HealthStatus::Healthy, HealthStatus::Healthy);
        assert_ne!(HealthStatus::Healthy, HealthStatus::Unhealthy);
        assert_eq!(
            HealthStatus::ProcessExited(Some(1)),
            HealthStatus::ProcessExited(Some(1))
        );
    }

    #[test]
    fn synapse_error_display() {
        assert_eq!(
            SynapseError::AlreadyRunning.to_string(),
            "Synapse is already running"
        );
        assert_eq!(
            SynapseError::NotRunning.to_string(),
            "Synapse is not running"
        );
        assert!(SynapseError::SpawnFailed("oops".into())
            .to_string()
            .contains("oops"));
        assert!(SynapseError::Io("disk full".into())
            .to_string()
            .contains("disk full"));
    }

    #[test]
    fn synapse_manager_not_running_by_default() {
        let config = SynapseConfig::default();
        let mgr = SynapseManager::new(config);
        assert!(!mgr.is_running());
    }
}
