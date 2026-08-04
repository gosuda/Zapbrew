use std::fs;

use camino::Utf8Path;
use zapbrew_api::RefreshReport;
use zapbrew_prefix::CommandSpec;

use crate::platform::{git_pull, run_checked};
use crate::{Ctx, OpError, tap};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {}

pub async fn run(ctx: &Ctx, _args: Args) -> Result<(), OpError> {
    ctx.reporter.ohai("Updating Homebrew...");
    let report = zapbrew_api::force_refresh(&ctx.env, &ctx.http).await?;
    finish(ctx, &report)
}

fn finish(ctx: &Ctx, report: &RefreshReport) -> Result<(), OpError> {
    for warning in &report.warnings {
        ctx.reporter.opoo(&warning.to_string());
    }

    let changed_taps = update_taps(ctx)?;
    if report.formulae_changed == 0 && !report.casks_changed && changed_taps.is_empty() {
        ctx.reporter.print("Already up-to-date.");
        return Ok(());
    }

    if !changed_taps.is_empty() {
        let noun = if changed_taps.len() == 1 {
            "tap"
        } else {
            "taps"
        };
        ctx.reporter.print(&format!(
            "Updated {} {noun} ({}).",
            changed_taps.len(),
            changed_taps.join(", ")
        ));
    }
    if report.formulae_changed > 0 {
        ctx.reporter.ohai("Updated Formulae");
        let noun = if report.formulae_changed == 1 {
            "formula"
        } else {
            "formulae"
        };
        ctx.reporter
            .print(&format!("Updated {} {noun}.", report.formulae_changed));
    }
    Ok(())
}

fn update_taps(ctx: &Ctx) -> Result<Vec<String>, OpError> {
    let mut changed = Vec::new();
    for tap in tap::installed(&ctx.env)? {
        let path = tap.path(&ctx.env);
        let git_dir = path.join(".git");
        match is_real_directory(&git_dir) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                warn(ctx, tap.name(), &error);
                continue;
            }
        }

        let before = match read_head(ctx, &path) {
            Ok(head) => head,
            Err(error) => {
                warn(ctx, tap.name(), &error);
                continue;
            }
        };
        if let Err(error) = run_checked(ctx.commands.as_ref(), &git_pull(&path)) {
            warn(ctx, tap.name(), &error);
            continue;
        }
        let after = match read_head(ctx, &path) {
            Ok(head) => head,
            Err(error) => {
                warn(ctx, tap.name(), &error);
                continue;
            }
        };
        if before != after {
            changed.push(tap.name().to_owned());
        }
    }
    changed.sort();
    Ok(changed)
}

fn read_head(ctx: &Ctx, tap: &Utf8Path) -> Result<String, OpError> {
    let output = run_checked(ctx.commands.as_ref(), &git_rev_parse_head(tap))?;
    Ok(String::from_utf8_lossy(output.stdout()).trim().to_owned())
}

fn git_rev_parse_head(tap: &Utf8Path) -> CommandSpec {
    CommandSpec::new("git")
        .arg("-C")
        .arg(tap.as_str())
        .args(["rev-parse", "HEAD"])
}

fn is_real_directory(path: &Utf8Path) -> Result<bool, OpError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(OpError::io(
            "inspect tap git directory",
            path.to_path_buf(),
            source,
        )),
    }
}

fn warn(ctx: &Ctx, tap: &str, error: &OpError) {
    ctx.reporter.opoo(&format!("{tap}: update failed: {error}"));
}

#[doc(hidden)]
pub(crate) fn run_with_report(ctx: &Ctx, report: &RefreshReport) -> Result<(), OpError> {
    ctx.reporter.ohai("Updating Homebrew...");
    finish(ctx, report)
}
