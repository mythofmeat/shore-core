use std::path::PathBuf;

use shore_mcp::profile::{resolve_profile, ProfileKind};

#[test]
fn attach_main_uses_main_profile_with_no_overrides() {
    let profile = resolve_profile(shore_mcp::cli::Cli {
        attach_main: true,
        ephemeral: false,
        allow_main_writes: false,
        daemon_addr: None,
    })
    .unwrap();

    assert_eq!(profile.kind, ProfileKind::Main);
    assert!(!profile.is_test());
    assert!(profile.env_overrides.is_empty());
}

#[test]
fn default_mode_uses_persistent_test_paths() {
    let profile = resolve_profile(shore_mcp::cli::Cli {
        attach_main: false,
        ephemeral: false,
        allow_main_writes: false,
        daemon_addr: None,
    })
    .unwrap();

    assert_eq!(profile.kind, ProfileKind::PersistentTest);
    assert!(profile.is_test());
    // Must export all three env vars.
    let keys: Vec<_> = profile.env_overrides.iter().map(|(k, _)| k.clone()).collect();
    assert!(keys.contains(&"SHORE_CONFIG_DIR".to_string()));
    assert!(keys.contains(&"SHORE_DATA_DIR".to_string()));
    assert!(keys.contains(&"SHORE_RUNTIME_DIR".to_string()));

    // All three paths should share a common ancestor named "shore-mcp-test".
    for (_, path) in &profile.env_overrides {
        assert!(
            PathBuf::from(path)
                .components()
                .any(|c| c.as_os_str() == "shore-mcp-test"),
            "expected shore-mcp-test in path: {path}"
        );
    }
}

#[test]
fn ephemeral_mode_uses_tempdir() {
    let profile = resolve_profile(shore_mcp::cli::Cli {
        attach_main: false,
        ephemeral: true,
        allow_main_writes: false,
        daemon_addr: None,
    })
    .unwrap();

    assert_eq!(profile.kind, ProfileKind::Ephemeral);
    assert!(profile.is_test());
    assert!(profile.tempdir.is_some(), "ephemeral profile must own a tempdir");
}
