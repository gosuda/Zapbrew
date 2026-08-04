mod cli;

use clap::Parser;

fn main() -> std::process::ExitCode {
    let _cli = cli::Cli::parse();
    std::process::ExitCode::SUCCESS
}
