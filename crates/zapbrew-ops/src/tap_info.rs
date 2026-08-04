use camino::Utf8Path;
use zapbrew_prefix::CommandSpec;

use crate::platform::run_checked;
use crate::size::disk_usage_readable;
use crate::tap::{TapName, TapStats, installed, is_installed, measure};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub installed: bool,
    pub json: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    if args.json {
        return Err(OpError::Refusal {
            message: "tap-info JSON output is unavailable without Ruby.".to_owned(),
        });
    }

    if args.names.is_empty() && !args.installed {
        return print_summary(ctx);
    }

    let mut taps = if args.installed {
        installed(&ctx.env)?
    } else {
        args.names
            .iter()
            .map(|name| TapName::parse(name))
            .collect::<Result<Vec<_>, _>>()?
    };
    taps.sort();

    let mut missing = 0_usize;
    for (index, tap) in taps.iter().enumerate() {
        if index != 0 {
            ctx.reporter.print("");
        }
        if !is_installed(&ctx.env, tap)? {
            ctx.reporter
                .print(&format!("{}: Not installed", tap.name()));
            missing += 1;
            continue;
        }
        print_installed(ctx, tap)?;
    }

    if missing == 0 {
        Ok(())
    } else {
        Err(OpError::Refusal {
            message: "One or more requested taps are not installed.".to_owned(),
        })
    }
}

fn print_summary(ctx: &Ctx) -> Result<(), OpError> {
    let taps = installed(&ctx.env)?;
    let mut formulae = 0_u64;
    let mut commands = 0_u64;
    let mut bytes = 0_u64;
    for tap in &taps {
        let stats = measure(&tap.path(&ctx.env))?;
        formulae += stats.formulae;
        commands += stats.commands;
        bytes = bytes.saturating_add(stats.bytes);
    }
    ctx.reporter.print(&format!(
        "{} taps, 0 private, {} formulae, {} commands, {}",
        taps.len(),
        formulae,
        commands,
        disk_usage_readable(bytes)
    ));
    Ok(())
}

fn print_installed(ctx: &Ctx, tap: &TapName) -> Result<(), OpError> {
    let path = tap.path(&ctx.env);
    let stats = measure(&path)?;
    let origin = git_value(
        ctx,
        &git(&path, &["config", "--get", "remote.origin.url"]),
        "(none)",
    );
    let head = git_value(ctx, &git(&path, &["rev-parse", "HEAD"]), "(none)");
    let last_commit = git_value(ctx, &git(&path, &["log", "-1", "--format=%cr"]), "never");
    let branch = git_value(
        ctx,
        &git(&path, &["symbolic-ref", "--short", "HEAD"]),
        "(none)",
    );

    let mut block = format!("{}: Installed\n{}", tap.name(), contents(stats));
    block.push_str(&format!(
        "\n{} ({})\norigin: {}\nHEAD: {}\nlast commit: {}",
        path,
        stats.abv(),
        origin,
        head,
        last_commit
    ));
    if !matches!(branch.as_str(), "main" | "master") {
        block.push_str(&format!("\nbranch: {branch}"));
    }
    ctx.reporter.print(&block);
    Ok(())
}

fn contents(stats: TapStats) -> String {
    let mut parts = Vec::new();
    if stats.commands > 0 {
        parts.push(plural(stats.commands, "command"));
    }
    if stats.casks > 0 {
        parts.push(plural(stats.casks, "cask"));
    }
    if stats.formulae > 0 {
        parts.push(plural(stats.formulae, "formula"));
    }
    if parts.is_empty() {
        "No commands/casks/formulae".to_owned()
    } else {
        parts.join(", ")
    }
}

fn plural(count: u64, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn git(path: &Utf8Path, arguments: &[&str]) -> CommandSpec {
    let mut spec = CommandSpec::new("git").arg("-C").arg(path.as_str());
    for argument in arguments {
        spec = spec.arg(argument);
    }
    spec
}

fn git_value(ctx: &Ctx, spec: &CommandSpec, fallback: &str) -> String {
    run_checked(ctx.commands.as_ref(), spec)
        .ok()
        .and_then(|output| {
            let value = String::from_utf8_lossy(output.stdout()).trim().to_owned();
            (!value.is_empty()).then_some(value)
        })
        .unwrap_or_else(|| fallback.to_owned())
}
