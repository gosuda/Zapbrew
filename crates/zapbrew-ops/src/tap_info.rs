use serde_json::json;

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
    if args.names.is_empty() && !args.installed {
        if args.json {
            return print_json_all(ctx);
        }
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

    if args.json {
        let mut missing = 0_usize;
        for tap in &taps {
            if !is_installed(&ctx.env, tap)? {
                missing += 1;
            }
        }
        if missing > 0 {
            return Err(OpError::Refusal {
                message: "One or more requested taps are not installed.".to_owned(),
            });
        }
        return print_json(ctx, &taps);
    }

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

fn tap_json(ctx: &Ctx, tap: &TapName) -> Result<serde_json::Value, OpError> {
    let path = tap.path(&ctx.env);
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
    let (_formula_dir, formula_files, formula_names) = formula_files(&path)?;
    let (cask_files, cask_tokens) = cask_files(&path)?;
    let command_files = command_files(&path)?;
    let custom_remote = origin != "(none)" && origin != default_remote(tap);
    Ok(json!({
        "name": tap.name(),
        "user": tap.user(),
        "repo": tap.repository(),
        "repository": tap.repository(),
        "path": path.as_str(),
        "installed": true,
        "official": tap.user() == "Homebrew",
        "remote": origin,
        "custom_remote": custom_remote,
        "HEAD": head,
        "last_commit": last_commit,
        "branch": branch,
        "formula_files": formula_files,
        "cask_files": cask_files,
        "command_files": command_files,
        "formula_names": formula_names,
        "cask_tokens": cask_tokens,
    }))
}
type FormulaFiles = (Option<String>, Vec<String>, Vec<String>);

fn formula_files(root: &Utf8Path) -> Result<FormulaFiles, OpError> {
    let subdir = if is_dir(&root.join("Formula")) {
        Some("Formula")
    } else if is_dir(&root.join("HomebrewFormula")) {
        Some("HomebrewFormula")
    } else if is_dir(root) {
        Some("")
    } else {
        None
    };
    let Some(prefix) = subdir else {
        return Ok((None, Vec::new(), Vec::new()));
    };
    let dir = if prefix.is_empty() {
        root.to_path_buf()
    } else {
        root.join(prefix)
    };
    let mut files = Vec::new();
    let mut names = Vec::new();
    for (relative, _) in sorted_rb_files(&dir, !prefix.is_empty())? {
        if prefix.is_empty() {
            files.push(relative.clone());
        } else {
            files.push(format!("{prefix}/{relative}"));
        }
        names.push(rb_basename(file_name(&relative)));
    }
    Ok((prefix.to_owned().into(), files, names))
}

fn cask_files(root: &Utf8Path) -> Result<(Vec<String>, Vec<String>), OpError> {
    let dir = root.join("Casks");
    if !is_dir(&dir) {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut files = Vec::new();
    let mut tokens = Vec::new();
    for (relative, _) in sorted_rb_files(&dir, true)? {
        files.push(format!("Casks/{relative}"));
        tokens.push(rb_basename(file_name(&relative)));
    }
    Ok((files, tokens))
}

fn command_files(root: &Utf8Path) -> Result<Vec<String>, OpError> {
    let dir = root.join("cmd");
    if !is_dir(&dir) {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let read_dir = std::fs::read_dir(dir.as_std_path())
        .map_err(|source| OpError::io("read directory", dir.clone(), source))?;
    for entry in read_dir.flatten() {
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("brew-") {
            files.push(format!("cmd/{name}"));
        }
    }
    files.sort();
    Ok(files)
}

fn sorted_rb_files(dir: &Utf8Path, recursive: bool) -> Result<Vec<(String, String)>, OpError> {
    let mut entries = Vec::new();
    collect_rb_files(dir, dir, recursive, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(entries)
}

fn collect_rb_files(
    root: &Utf8Path,
    dir: &Utf8Path,
    recursive: bool,
    out: &mut Vec<(String, String)>,
) -> Result<(), OpError> {
    let read_dir = std::fs::read_dir(dir.as_std_path())
        .map_err(|source| OpError::io("read directory", dir.to_path_buf(), source))?;
    for entry in read_dir.flatten() {
        let Ok(metadata) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if metadata.is_dir() && recursive {
            let child = dir.join(&name);
            collect_rb_files(root, &child, recursive, out)?;
        } else if metadata.is_file() && name.ends_with(".rb") {
            let relative = entry
                .path()
                .strip_prefix(root.as_std_path())
                .map_err(|_| OpError::InvalidState {
                    reason: format!("path not under root: {}", entry.path().display()),
                })?
                .to_string_lossy()
                .into_owned();
            out.push((relative, name));
        }
    }
    Ok(())
}

fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

fn is_dir(path: &Utf8Path) -> bool {
    std::fs::symlink_metadata(path.as_std_path()).is_ok_and(|m| m.is_dir())
}

fn rb_basename(name: &str) -> String {
    name.strip_suffix(".rb").unwrap_or(name).to_owned()
}

fn default_remote(tap: &TapName) -> String {
    format!(
        "https://github.com/{}/homebrew-{}",
        tap.user(),
        tap.repository()
    )
}
fn print_json(ctx: &Ctx, taps: &[TapName]) -> Result<(), OpError> {
    let hashes: Vec<serde_json::Value> = taps
        .iter()
        .map(|t| tap_json(ctx, t))
        .collect::<Result<_, _>>()?;
    let payload = serde_json::to_string_pretty(&hashes).map_err(|e| OpError::InvalidState {
        reason: format!("serialize tap-info JSON: {e}"),
    })?;
    ctx.reporter.print(&payload);
    Ok(())
}

fn print_json_all(ctx: &Ctx) -> Result<(), OpError> {
    let taps = installed(&ctx.env)?;
    print_json(ctx, &taps)
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
