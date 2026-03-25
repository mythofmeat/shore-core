mod cli;
mod output;
mod run;

use std::process::ExitCode;

use cli::Cli;

fn main() -> ExitCode {
    let cli = <Cli as clap::Parser>::parse();

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
