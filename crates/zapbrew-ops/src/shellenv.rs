use std::path::Path;

use zapbrew_prefix::Shell;

use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub shell: Option<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let detected = std::env::var_os("SHELL");
    run_with_detected(ctx, args, detected.as_deref()).await
}

pub(crate) async fn run_with_detected(
    ctx: &Ctx,
    args: Args,
    detected: Option<&std::ffi::OsStr>,
) -> Result<(), OpError> {
    let shell = args
        .shell
        .as_deref()
        .map_or_else(|| detected.and_then(|value| value.to_str()), Some);
    ctx.reporter.print(&ctx.env.shellenv(select_shell(shell)));
    Ok(())
}

fn select_shell(value: Option<&str>) -> Shell {
    let basename = value
        .and_then(|value| Path::new(value).file_name())
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let name = basename.strip_prefix('-').unwrap_or(basename);

    match name {
        "fish" => Shell::Fish,
        "csh" | "tcsh" => Shell::Csh,
        "pwsh" | "pwsh-preview" => Shell::Pwsh,
        "zsh" => Shell::Zsh,
        "bash" | "sh" => Shell::Bash,
        _ => Shell::Bash,
    }
}
