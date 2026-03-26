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
