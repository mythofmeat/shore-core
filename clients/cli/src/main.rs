mod cli;
mod images;
mod output;
mod run;
mod state;

use std::process::ExitCode;

use cli::{Cli, CliCommand};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    let cli = <Cli as clap::Parser>::parse();

    // Completion queries must never print to stderr — fish feeds both
    // streams into the prompt, and any stray tracing line would be
    // offered as a candidate. Silence everything for this one path.
    let default_filter = if matches!(cli.command, CliCommand::Complete { .. }) {
        "off"
    } else {
        "warn"
    };

    // CLI logs to stderr so stdout stays clean for command output.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .with_target(true)
        .with_writer(std::io::stderr)
        .init();

    // Initialize color control: --no-color flag or NO_COLOR env var disables color.
    let no_color = cli.no_color
        || std::env::var("NO_COLOR")
            .map(|v| !v.is_empty())
            .unwrap_or(false);
    output::set_color_enabled(!no_color);

    // Handle local-only commands that don't need a daemon connection.
    if let CliCommand::Completions { shell } = &cli.command {
        cli::print_completions(*shell);
        return ExitCode::SUCCESS;
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");

    match rt.block_on(run::execute(cli)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            output::print_error(&e);
            ExitCode::FAILURE
        }
    }
}
