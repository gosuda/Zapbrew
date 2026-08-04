mod cli;
pub mod output;

use clap::Parser;

fn main() -> std::process::ExitCode {
    let _cli = cli::Cli::parse();
    output::success()
}
