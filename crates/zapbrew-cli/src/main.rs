//! `zapbrew` entry point.
//!
//! Sync `main` parses the clap surface, detects the host [`Env`] once, folds the
//! clap debug/verbose globals into it, and resolves top-level fast paths with
//! zero network before any runtime. A real command spins a Tokio multi-thread
//! runtime and drives [`dispatch::run`], which owns catalog loading, the `Ctx`,
//! SIGINT handling, and the exit contract.

mod cli;
mod completions;
mod dispatch;
mod fastpath;
pub mod output;

use std::process::ExitCode;
use std::sync::Arc;

use clap::{CommandFactory, Parser};
use zapbrew_ops::Reporter;
use zapbrew_prefix::Env;

use crate::fastpath::FastPath;
use crate::output::TerminalReporter;

fn main() -> ExitCode {
    let args = cli::canonicalize_argv(std::env::args().collect());
    let cli = cli::Cli::parse_from(args);

    // `completions generate` and the legacy `completions <shell>` shortcut are
    // pure functions of the clap surface. Intercept them before `Env::detect`
    // so they never touch the host or the network.
    if let Some(cli::Commands::Completions(args)) = &cli.command {
        let shell = args.shell.or(match &args.command {
            Some(cli::CompletionsCommand::Generate { shell }) => Some(*shell),
            _ => None,
        });
        if let Some(shell) = shell {
            completions::generate(shell, &mut std::io::stdout());
            return output::success();
        }
    }

    let argv0 = std::env::args().next().unwrap_or_default();
    let hint = zapbrew_ops::shim::hint_program(&argv0);

    let mut env = match Env::detect() {
        Ok(env) => env,
        Err(err) => {
            // The one error raised before a reporter can exist: reporting needs
            // the environment we just failed to detect.
            eprintln!("Error: {err}");
            return ExitCode::FAILURE;
        }
    };
    env.debug |= cli.globals.debug;
    env.verbose |= cli.globals.verbose;

    let reporter = TerminalReporter::from_env(&env, cli.globals.quiet, env.verbose, hint);

    // Completions state/link/unlink are handled here, before any runtime,
    // catalog, or network work. They only inspect and mutate the active prefix.
    if let Some(cli::Commands::Completions(args)) = &cli.command {
        match completions::run(args, &env, &reporter) {
            Ok(()) => return output::success(),
            Err(err) => {
                output::report_error(&reporter, &err);
                return output::exit_code(&err);
            }
        }
    }

    match fastpath::resolve(&cli, &env) {
        FastPath::Print(line) => {
            reporter.print(&line);
            return output::success();
        }
        FastPath::Refuse(message) => {
            reporter.onoe(&message);
            return ExitCode::FAILURE;
        }
        FastPath::None => {}
    }

    if cli.command.is_none() {
        // Globals with no command or path query: clap's arg_required_else_help
        // usually pre-empts this, so print the long help and succeed.
        let _ = cli::Cli::command().print_help();
        return output::success();
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            reporter.onoe(&format!("failed to start the async runtime: {err}"));
            return ExitCode::FAILURE;
        }
    };

    let reporter: Arc<dyn Reporter> = Arc::new(reporter);
    runtime.block_on(dispatch::run(cli, env, reporter))
}
