mod cli;
mod images;
mod notifications;
mod output;
mod run;
mod state;

use std::process::ExitCode;

use cli::{Cli, CliCommand};

fn main() -> ExitCode {
    let cli = <Cli as clap::Parser>::parse();

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
