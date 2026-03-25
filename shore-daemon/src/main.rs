use shore_daemon::compat;
use std::path::PathBuf;

/// Resolve the config directory: `$XDG_CONFIG_HOME/shore/` or `~/.config/shore/`.
fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .expect("cannot determine home directory")
                .join(".config")
        })
        .join("shore")
}

fn main() {
    let config_dir = config_dir();
    let config_path = config_dir.join("config.toml");
    let models_path = config_dir.join("models.toml");

    // Load config, printing V1 migration warnings if deprecated sections are found.
    let config = if config_path.exists() {
        match compat::parse_config(&config_path) {
            Ok(result) => {
                for warning in &result.warnings {
                    eprintln!("[migration] {warning}");
                }
                result.config
            }
            Err(e) => {
                eprintln!("error: failed to parse {}: {e}", config_path.display());
                std::process::exit(1);
            }
        }
    } else {
        compat::Config::default()
    };

    // Load models, printing V1 migration warnings for deprecated field names.
    if models_path.exists() {
        match compat::parse_models(&models_path) {
            Ok(result) => {
                for warning in &result.warnings {
                    eprintln!("[migration] {warning}");
                }
            }
            Err(e) => {
                eprintln!("error: failed to parse {}: {e}", models_path.display());
                std::process::exit(1);
            }
        }
    }

    println!(
        "shore-daemon starting (character: {}, socket: {})",
        config.character.name, config.daemon.socket_path,
    );
}
