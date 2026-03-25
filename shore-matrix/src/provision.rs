use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Persisted provisioning state for a character's Matrix account.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProvisionState {
    /// Character name
    pub character: String,
    /// Matrix user ID (e.g. @shore-alice:localhost)
    pub user_id: String,
    /// The device ID used by this bot account
    pub device_id: String,
    /// Access token for the bot account
    pub access_token: String,
    /// Room ID the character is provisioned into
    pub room_id: Option<String>,
    /// Whether the avatar has been set
    pub avatar_set: bool,
    /// Homeserver URL used during provisioning
    pub homeserver_url: String,
}

/// Paths for a character's Matrix data within the XDG data directory.
#[derive(Debug, Clone)]
pub struct CharacterPaths {
    /// Root: $XDG_DATA_HOME/shore/{character}/matrix/
    pub matrix_dir: PathBuf,
    /// $XDG_DATA_HOME/shore/{character}/matrix/provision.json
    pub provision_file: PathBuf,
    /// $XDG_DATA_HOME/shore/{character}/matrix/crypto_store/
    pub crypto_store: PathBuf,
    /// $XDG_DATA_HOME/shore/{character}/ (for avatar lookup)
    pub character_dir: PathBuf,
}

impl CharacterPaths {
    /// Compute paths for a character, using the standard XDG data directory.
    pub fn new(character: &str) -> Self {
        let data_dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".local/share"));
        Self::with_base(data_dir, character)
    }

    /// Compute paths with an explicit base data directory (useful for testing).
    pub fn with_base(base: PathBuf, character: &str) -> Self {
        let character_dir = base.join("shore").join(character);
        let matrix_dir = character_dir.join("matrix");
        let provision_file = matrix_dir.join("provision.json");
        let crypto_store = matrix_dir.join("crypto_store");
        Self {
            matrix_dir,
            provision_file,
            crypto_store,
            character_dir,
        }
    }

    /// Create all required directories.
    pub async fn ensure_dirs(&self) -> Result<(), ProvisionError> {
        tokio::fs::create_dir_all(&self.matrix_dir)
            .await
            .map_err(|e| ProvisionError::Io(format!("create matrix dir: {e}")))?;
        tokio::fs::create_dir_all(&self.crypto_store)
            .await
            .map_err(|e| ProvisionError::Io(format!("create crypto store: {e}")))?;
        Ok(())
    }
}

impl ProvisionState {
    /// Load provisioning state from a JSON file.
    pub fn load(path: &Path) -> Result<Option<Self>, ProvisionError> {
        if !path.exists() {
            return Ok(None);
        }
        let data = std::fs::read_to_string(path)
            .map_err(|e| ProvisionError::Io(format!("read provision.json: {e}")))?;
        let state: Self = serde_json::from_str(&data)
            .map_err(|e| ProvisionError::InvalidState(format!("parse provision.json: {e}")))?;
        Ok(Some(state))
    }

    /// Save provisioning state to a JSON file.
    pub fn save(&self, path: &Path) -> Result<(), ProvisionError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ProvisionError::Io(format!("create parent dir: {e}")))?;
        }
        let data = serde_json::to_string_pretty(self)
            .map_err(|e| ProvisionError::Io(format!("serialize provision state: {e}")))?;
        std::fs::write(path, data)
            .map_err(|e| ProvisionError::Io(format!("write provision.json: {e}")))?;
        info!("saved provision state for {}", self.character);
        Ok(())
    }

    /// Async variant of save.
    pub async fn save_async(&self, path: &Path) -> Result<(), ProvisionError> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| ProvisionError::Io(format!("create parent dir: {e}")))?;
        }
        let data = serde_json::to_string_pretty(self)
            .map_err(|e| ProvisionError::Io(format!("serialize provision state: {e}")))?;
        tokio::fs::write(path, data)
            .await
            .map_err(|e| ProvisionError::Io(format!("write provision.json: {e}")))?;
        info!("saved provision state for {}", self.character);
        Ok(())
    }
}

/// Register a Matrix account using Synapse's shared-secret registration endpoint.
///
/// This uses the `/_synapse/admin/v1/register` API which requires an HMAC
/// signature computed from the registration shared secret.
pub async fn register_account(
    homeserver_url: &str,
    shared_secret: &str,
    username: &str,
    password: &str,
    admin: bool,
) -> Result<RegisterResponse, ProvisionError> {
    let client = reqwest::Client::new();

    // Step 1: Get nonce
    let nonce_url = format!("{homeserver_url}/_synapse/admin/v1/register");
    let nonce_resp: NonceResponse = client
        .get(&nonce_url)
        .send()
        .await
        .map_err(|e| ProvisionError::Http(format!("get nonce: {e}")))?
        .json()
        .await
        .map_err(|e| ProvisionError::Http(format!("parse nonce: {e}")))?;

    // Step 2: Compute HMAC
    let mac = compute_registration_mac(
        shared_secret,
        &nonce_resp.nonce,
        username,
        password,
        admin,
    );

    // Step 3: Register
    let body = serde_json::json!({
        "nonce": nonce_resp.nonce,
        "username": username,
        "password": password,
        "admin": admin,
        "mac": mac,
    });

    let resp = client
        .post(&nonce_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| ProvisionError::Http(format!("register: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(ProvisionError::Registration(format!(
            "status {status}: {body}"
        )));
    }

    let result: RegisterResponse = resp
        .json()
        .await
        .map_err(|e| ProvisionError::Http(format!("parse register response: {e}")))?;

    info!("registered Matrix account: {}", result.user_id);
    Ok(result)
}

/// Compute the HMAC for Synapse shared-secret registration.
///
/// Format: HMAC-SHA1(shared_secret, nonce + "\0" + username + "\0" + password + "\0" + admin_flag)
fn compute_registration_mac(
    shared_secret: &str,
    nonce: &str,
    username: &str,
    password: &str,
    admin: bool,
) -> String {
    use hmac::{Hmac, Mac};
    use sha1::Sha1;

    type HmacSha1 = Hmac<Sha1>;

    let admin_str = if admin { "admin" } else { "notadmin" };
    let message = format!("{nonce}\0{username}\0{password}\0{admin_str}");

    let mut mac =
        HmacSha1::new_from_slice(shared_secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(message.as_bytes());
    let result = mac.finalize();
    hex::encode(result.into_bytes())
}

/// Full provisioning flow for a character.
///
/// 1. Load existing state (skip if already provisioned)
/// 2. Register Matrix account via admin API
/// 3. Save provision state
pub async fn provision_character(
    homeserver_url: &str,
    shared_secret: &str,
    character: &str,
    password: &str,
    paths: &CharacterPaths,
) -> Result<ProvisionState, ProvisionError> {
    // Check for existing provisioning
    if let Some(state) = ProvisionState::load(&paths.provision_file)? {
        if state.homeserver_url == homeserver_url {
            info!(
                "character {} already provisioned as {}",
                character, state.user_id
            );
            return Ok(state);
        }
        warn!(
            "character {} provisioned for different homeserver ({}), re-provisioning",
            character, state.homeserver_url
        );
    }

    paths.ensure_dirs().await?;

    // Register the bot account (not admin)
    let username = format!("shore-{}", character.to_lowercase().replace(' ', "-"));
    let reg = register_account(homeserver_url, shared_secret, &username, password, false).await?;

    let state = ProvisionState {
        character: character.to_string(),
        user_id: reg.user_id,
        device_id: reg.device_id.unwrap_or_else(|| "SHORE_MATRIX".to_string()),
        access_token: reg.access_token,
        room_id: None,
        avatar_set: false,
        homeserver_url: homeserver_url.to_string(),
    };

    state.save_async(&paths.provision_file).await?;
    Ok(state)
}

/// Provision the admin account on first run.
pub async fn provision_admin(
    homeserver_url: &str,
    shared_secret: &str,
    admin_password: &str,
) -> Result<RegisterResponse, ProvisionError> {
    register_account(
        homeserver_url,
        shared_secret,
        "shore-admin",
        admin_password,
        true,
    )
    .await
}

#[derive(Debug, Deserialize)]
struct NonceResponse {
    nonce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub user_id: String,
    pub access_token: String,
    pub device_id: Option<String>,
    pub home_server: Option<String>,
}

#[derive(Debug)]
pub enum ProvisionError {
    Io(String),
    InvalidState(String),
    Http(String),
    Registration(String),
}

impl std::fmt::Display for ProvisionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::InvalidState(e) => write!(f, "invalid provision state: {e}"),
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::Registration(e) => write!(f, "registration failed: {e}"),
        }
    }
}

impl std::error::Error for ProvisionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn provision_state_roundtrip() {
        let state = ProvisionState {
            character: "Alice".to_string(),
            user_id: "@shore-alice:localhost".to_string(),
            device_id: "DEV123".to_string(),
            access_token: "tok_abc".to_string(),
            room_id: Some("!room:localhost".to_string()),
            avatar_set: true,
            homeserver_url: "http://localhost:8008".to_string(),
        };

        let json = serde_json::to_string_pretty(&state).unwrap();
        let restored: ProvisionState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, restored);
    }

    #[test]
    fn provision_state_save_and_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("provision.json");

        let state = ProvisionState {
            character: "Bob".to_string(),
            user_id: "@shore-bob:localhost".to_string(),
            device_id: "DEV456".to_string(),
            access_token: "tok_xyz".to_string(),
            room_id: None,
            avatar_set: false,
            homeserver_url: "http://localhost:8008".to_string(),
        };

        state.save(&path).unwrap();
        let loaded = ProvisionState::load(&path).unwrap().unwrap();
        assert_eq!(state, loaded);
    }

    #[test]
    fn provision_state_load_nonexistent() {
        let result = ProvisionState::load(Path::new("/nonexistent/provision.json")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn provision_state_load_invalid_json() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("provision.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"not json").unwrap();

        let result = ProvisionState::load(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("parse provision.json"));
    }

    #[test]
    fn provision_state_json_fields() {
        let state = ProvisionState {
            character: "Eve".to_string(),
            user_id: "@shore-eve:matrix.org".to_string(),
            device_id: "SHORE_MATRIX".to_string(),
            access_token: "secret_token".to_string(),
            room_id: Some("!abc:matrix.org".to_string()),
            avatar_set: false,
            homeserver_url: "https://matrix.org".to_string(),
        };

        let json: serde_json::Value = serde_json::to_value(&state).unwrap();
        assert_eq!(json["character"], "Eve");
        assert_eq!(json["user_id"], "@shore-eve:matrix.org");
        assert_eq!(json["device_id"], "SHORE_MATRIX");
        assert_eq!(json["access_token"], "secret_token");
        assert_eq!(json["room_id"], "!abc:matrix.org");
        assert_eq!(json["avatar_set"], false);
        assert_eq!(json["homeserver_url"], "https://matrix.org");
    }

    #[test]
    fn character_paths_structure() {
        let base = PathBuf::from("/home/user/.local/share");
        let paths = CharacterPaths::with_base(base, "alice");

        assert_eq!(
            paths.character_dir,
            PathBuf::from("/home/user/.local/share/shore/alice")
        );
        assert_eq!(
            paths.matrix_dir,
            PathBuf::from("/home/user/.local/share/shore/alice/matrix")
        );
        assert_eq!(
            paths.provision_file,
            PathBuf::from("/home/user/.local/share/shore/alice/matrix/provision.json")
        );
        assert_eq!(
            paths.crypto_store,
            PathBuf::from("/home/user/.local/share/shore/alice/matrix/crypto_store")
        );
    }

    #[test]
    fn compute_hmac_known_value() {
        // Synapse uses HMAC-SHA1 for registration MAC
        let mac = compute_registration_mac("secret", "nonce123", "user", "pass", false);
        // Just verify it's a hex string of correct length (SHA1 = 20 bytes = 40 hex chars)
        assert_eq!(mac.len(), 40);
        assert!(mac.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn compute_hmac_admin_differs() {
        let mac1 = compute_registration_mac("secret", "nonce", "user", "pass", false);
        let mac2 = compute_registration_mac("secret", "nonce", "user", "pass", true);
        assert_ne!(mac1, mac2);
    }

    #[test]
    fn provision_error_display() {
        assert!(ProvisionError::Io("disk full".into())
            .to_string()
            .contains("disk full"));
        assert!(ProvisionError::InvalidState("bad json".into())
            .to_string()
            .contains("bad json"));
        assert!(ProvisionError::Http("timeout".into())
            .to_string()
            .contains("timeout"));
        assert!(ProvisionError::Registration("403".into())
            .to_string()
            .contains("403"));
    }

    #[test]
    fn register_response_deserialize() {
        let json = r#"{
            "user_id": "@shore-test:localhost",
            "access_token": "tok123",
            "device_id": "DEV",
            "home_server": "localhost"
        }"#;
        let resp: RegisterResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.user_id, "@shore-test:localhost");
        assert_eq!(resp.access_token, "tok123");
        assert_eq!(resp.device_id.as_deref(), Some("DEV"));
        assert_eq!(resp.home_server.as_deref(), Some("localhost"));
    }

    #[test]
    fn provision_state_save_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a").join("b").join("provision.json");

        let state = ProvisionState {
            character: "test".to_string(),
            user_id: "@test:localhost".to_string(),
            device_id: "DEV".to_string(),
            access_token: "tok".to_string(),
            room_id: None,
            avatar_set: false,
            homeserver_url: "http://localhost:8008".to_string(),
        };

        state.save(&path).unwrap();
        assert!(path.exists());
    }
}
