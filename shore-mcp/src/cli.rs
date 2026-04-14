use clap::Parser;

/// Shore MCP server — exposes the Shore CLI surface as MCP tools.
#[derive(Debug, Parser, Clone)]
#[command(name = "shore-mcp", version, about)]
pub struct Cli {
    /// Attach to the user's main Shore daemon profile instead of the
    /// isolated test profile. Mutation tools are refused in this mode
    /// unless `--allow-main-writes` is also set.
    #[arg(long)]
    pub attach_main: bool,

    /// Use a fresh tempdir profile instead of the persistent test profile
    /// at `$XDG_DATA_HOME/shore-mcp-test/`. Ignored if `--attach-main` is
    /// set. The tempdir and its spawned daemon are torn down on exit.
    #[arg(long, conflicts_with = "attach_main")]
    pub ephemeral: bool,

    /// Permit mutation tools to execute against the main profile. Requires
    /// `--attach-main`; a no-op otherwise. This is a deliberate two-flag
    /// opt-in, not a default.
    #[arg(long, requires = "attach_main")]
    pub allow_main_writes: bool,

    /// Override the daemon TCP address instead of discovering it. Useful
    /// for integration tests where the daemon is already running and its
    /// address is known. Mutually exclusive with the default spawn path.
    #[arg(long, value_name = "ADDR")]
    pub daemon_addr: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        let mut argv = vec!["shore-mcp"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv)
    }

    #[test]
    fn defaults_are_all_false() {
        let cli = parse(&[]).unwrap();
        assert!(!cli.attach_main);
        assert!(!cli.ephemeral);
        assert!(!cli.allow_main_writes);
        assert!(cli.daemon_addr.is_none());
    }

    #[test]
    fn attach_main_flag() {
        let cli = parse(&["--attach-main"]).unwrap();
        assert!(cli.attach_main);
    }

    #[test]
    fn ephemeral_and_attach_main_are_mutually_exclusive() {
        let err = parse(&["--ephemeral", "--attach-main"]).unwrap_err();
        assert!(err.to_string().contains("cannot be used with"));
    }

    #[test]
    fn allow_main_writes_requires_attach_main() {
        let err = parse(&["--allow-main-writes"]).unwrap_err();
        assert!(err.to_string().contains("required"));
    }

    #[test]
    fn allow_main_writes_accepted_with_attach_main() {
        let cli = parse(&["--attach-main", "--allow-main-writes"]).unwrap();
        assert!(cli.attach_main);
        assert!(cli.allow_main_writes);
    }

    #[test]
    fn daemon_addr_override() {
        let cli = parse(&["--daemon-addr", "127.0.0.1:7999"]).unwrap();
        assert_eq!(cli.daemon_addr.as_deref(), Some("127.0.0.1:7999"));
    }
}
