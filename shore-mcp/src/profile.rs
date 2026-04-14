use std::path::PathBuf;

use crate::cli::Cli;

/// Kind of profile we resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileKind {
    /// `--attach-main`: user's real daemon.
    Main,
    /// Default: persistent test profile at $XDG_DATA_HOME/shore-mcp-test.
    PersistentTest,
    /// `--ephemeral`: tempdir, torn down on exit.
    Ephemeral,
}

/// Resolved profile info. Consumers set env vars before spawning the daemon.
#[derive(Debug)]
pub struct ResolvedProfile {
    pub kind: ProfileKind,
    /// (env_var_name, value) pairs to export before starting shore-client
    /// discovery or spawning a daemon. Empty for `Main`.
    pub env_overrides: Vec<(String, String)>,
    /// Tempdir handle, only set for `Ephemeral`. Drop-on-exit keeps the
    /// profile directory alive for the lifetime of the MCP server.
    pub tempdir: Option<tempfile::TempDir>,
}

impl ResolvedProfile {
    /// Whether mutation tools are gated (i.e., this is NOT the main profile).
    pub fn is_test(&self) -> bool {
        !matches!(self.kind, ProfileKind::Main)
    }
}

/// Resolve which profile to use from parsed CLI args.
pub fn resolve_profile(cli: Cli) -> anyhow::Result<ResolvedProfile> {
    if cli.attach_main {
        return Ok(ResolvedProfile {
            kind: ProfileKind::Main,
            env_overrides: Vec::new(),
            tempdir: None,
        });
    }

    if cli.ephemeral {
        let td = tempfile::tempdir()?;
        let base = td.path().to_path_buf();
        let overrides = build_env_overrides(&base);
        return Ok(ResolvedProfile {
            kind: ProfileKind::Ephemeral,
            env_overrides: overrides,
            tempdir: Some(td),
        });
    }

    // Persistent test profile.
    let base = persistent_test_base();
    let overrides = build_env_overrides(&base);
    Ok(ResolvedProfile {
        kind: ProfileKind::PersistentTest,
        env_overrides: overrides,
        tempdir: None,
    })
}

/// Default location for the persistent test profile.
///
/// Uses `$XDG_DATA_HOME/shore-mcp-test/` or `$HOME/.local/share/shore-mcp-test/`
/// as a fallback. Never returns a path inside the user's real Shore profile.
fn persistent_test_base() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("shore-mcp-test");
        }
    }
    if let Some(home) = dirs::home_dir() {
        return home.join(".local").join("share").join("shore-mcp-test");
    }
    // Last-resort fallback. If HOME is unset the user has bigger problems.
    PathBuf::from("/tmp/shore-mcp-test")
}

fn build_env_overrides(base: &std::path::Path) -> Vec<(String, String)> {
    let config = base.join("config");
    let data = base.join("data");
    let runtime = base.join("runtime");
    vec![
        (
            "SHORE_CONFIG_DIR".into(),
            config.to_string_lossy().into_owned(),
        ),
        (
            "SHORE_DATA_DIR".into(),
            data.to_string_lossy().into_owned(),
        ),
        (
            "SHORE_RUNTIME_DIR".into(),
            runtime.to_string_lossy().into_owned(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_cli() -> Cli {
        Cli {
            attach_main: false,
            ephemeral: false,
            allow_main_writes: false,
            daemon_addr: None,
        }
    }

    #[test]
    fn main_profile_has_no_env_overrides() {
        let cli = Cli {
            attach_main: true,
            ..blank_cli()
        };
        let resolved = resolve_profile(cli).unwrap();
        assert_eq!(resolved.kind, ProfileKind::Main);
        assert!(resolved.env_overrides.is_empty());
        assert!(!resolved.is_test());
    }

    #[test]
    fn persistent_profile_under_xdg_data_home() {
        std::env::set_var("XDG_DATA_HOME", "/tmp/test-shore-mcp-xdg");
        let resolved = resolve_profile(blank_cli()).unwrap();
        assert_eq!(resolved.kind, ProfileKind::PersistentTest);
        for (_, path) in &resolved.env_overrides {
            assert!(path.starts_with("/tmp/test-shore-mcp-xdg/shore-mcp-test"));
        }
        std::env::remove_var("XDG_DATA_HOME");
    }

    #[test]
    fn ephemeral_profile_keeps_tempdir_alive() {
        let cli = Cli {
            ephemeral: true,
            ..blank_cli()
        };
        let resolved = resolve_profile(cli).unwrap();
        assert_eq!(resolved.kind, ProfileKind::Ephemeral);
        let tempdir_path = resolved.tempdir.as_ref().unwrap().path().to_path_buf();
        assert!(tempdir_path.exists());
        // Env overrides must live under the tempdir.
        for (_, path) in &resolved.env_overrides {
            assert!(path.starts_with(tempdir_path.to_str().unwrap()));
        }
    }
}
