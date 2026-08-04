use std::collections::BTreeSet;
use std::env;
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use rustix::fs::{Access, access};

use crate::{Ctx, OpError, cleanup};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Args;

pub async fn run(ctx: &Ctx, _args: Args) -> Result<(), OpError> {
    let path_entries = match env::var_os("PATH") {
        Some(path) => env::split_paths(&path)
            .filter_map(|entry| Utf8PathBuf::from_path_buf(entry).ok())
            .collect::<Vec<_>>(),
        None => Vec::new(),
    };
    report(
        ctx,
        findings(ctx, &path_entries, &|path| {
            access(path.as_std_path(), Access::WRITE_OK).is_ok()
        })?,
    );
    Ok(())
}

pub(crate) fn report(ctx: &Ctx, findings: Vec<String>) {
    if findings.is_empty() {
        ctx.reporter.print("Your system is ready to brew.");
    } else {
        for finding in findings {
            ctx.reporter.opoo(&finding);
        }
    }
}

pub(crate) fn findings(
    ctx: &Ctx,
    path_entries: &[Utf8PathBuf],
    writable: &dyn Fn(&Utf8Path) -> bool,
) -> Result<Vec<String>, OpError> {
    let mut findings = Vec::new();

    let prefix_usable = configured_root_usable(&ctx.env.prefix)?;
    let cache_usable = configured_root_usable(&ctx.env.cache)?;
    let cellar_usable = configured_root_usable(&ctx.env.cellar)?;
    let mut invalid_roots = Vec::new();
    if matches!(prefix_usable, RootUsability::Invalid) {
        invalid_roots.push(ctx.env.prefix.clone());
    }
    if matches!(cache_usable, RootUsability::Invalid) {
        invalid_roots.push(ctx.env.cache.clone());
    }
    if matches!(cellar_usable, RootUsability::Invalid) {
        invalid_roots.push(ctx.env.cellar.clone());
    }
    invalid_roots.sort();
    if !invalid_roots.is_empty() {
        findings.push(list_finding(
            "The following configured roots are not real directories:",
            &invalid_roots,
            "Replace each symlink or non-directory with a real directory.",
        ));
    }

    if !matches!(prefix_usable, RootUsability::Invalid) {
        let broken = cleanup::broken_prefix_symlinks(&ctx.env)?;
        if !broken.is_empty() {
            findings.push(list_finding(
                "Broken symlinks were found:",
                &broken,
                "Remove them with `brew cleanup`.",
            ));
        }

        if !matches!(cellar_usable, RootUsability::Invalid) {
            let unlinked = unlinked_racks(ctx)?;
            if !unlinked.is_empty() {
                findings.push(format!(
                    "You have unlinked kegs in your Cellar.\n\
                 Leaving kegs unlinked can lead to build-trouble and cause formulae that depend on\n\
                 those kegs to fail to run properly once built.\n\n\
                 Run `brew link` on these:\n{}",
                    indented(&unlinked)
                ));
            }
        }

        findings.extend(path_findings(&ctx.env.prefix, path_entries)?);
    }

    if !matches!(cache_usable, RootUsability::Invalid) {
        let incomplete = cleanup::cache_incomplete_entries(&ctx.env)?;
        if !incomplete.is_empty() {
            findings.push(list_finding(
                "Stray incomplete downloads were found:",
                &incomplete,
                "Remove them with `brew cleanup`.",
            ));
        }
    }

    let mut unwritable = Vec::new();
    if !matches!(cache_usable, RootUsability::Invalid) && !writable(&ctx.env.cache) {
        unwritable.push(ctx.env.cache.clone());
    }
    if !matches!(prefix_usable, RootUsability::Invalid) && !writable(&ctx.env.prefix) {
        unwritable.push(ctx.env.prefix.clone());
    }
    unwritable.sort();
    if !unwritable.is_empty() {
        findings.push(list_finding(
            "The following directories are not writable by your user:",
            &unwritable,
            "Change their ownership or grant your user write permission.",
        ));
    }

    findings.sort();
    Ok(findings)
}

fn unlinked_racks(ctx: &Ctx) -> Result<Vec<Utf8PathBuf>, OpError> {
    let mut racks = Vec::new();
    if !ctx.env.cellar.exists() {
        return Ok(racks);
    }
    let entries = fs::read_dir(&ctx.env.cellar)
        .map_err(|source| OpError::io("read directory", ctx.env.cellar.clone(), source))?;
    for entry in entries {
        let entry = entry
            .map_err(|source| OpError::io("read directory", ctx.env.cellar.clone(), source))?;
        let path =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("Cellar rack path is not UTF-8: {}", path.display()),
            })?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| OpError::io("inspect", path.clone(), source))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        if ctx
            .catalog
            .get(name)
            .is_some_and(|formula| formula.keg_only)
        {
            continue;
        }
        if fs::metadata(ctx.env.linked.join(name)).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        racks.push(path);
    }
    racks.sort();
    Ok(racks)
}

fn path_findings(prefix: &Utf8Path, entries: &[Utf8PathBuf]) -> Result<Vec<String>, OpError> {
    let prefix_bin = prefix.join("bin");
    let prefix_sbin = prefix.join("sbin");
    let bin_position = entries.iter().position(|entry| entry == &prefix_bin);
    let sbin_position = entries.iter().position(|entry| entry == &prefix_sbin);
    let mut findings = Vec::new();

    if let Some(position) = bin_position {
        let homebrew_tools = directory_names(&prefix_bin)?;
        for system_dir in &entries[..position] {
            let conflicts = directory_names(system_dir)?
                .intersection(&homebrew_tools)
                .cloned()
                .collect::<Vec<_>>();
            if conflicts.is_empty() {
                continue;
            }
            findings.push(format!(
                "{system_dir} occurs before {prefix_bin} in your PATH.\n\
                 This means that system-provided programs will be used instead of those\n\
                 provided by Homebrew.\n\n\
                 The following tools exist at both paths:\n{}",
                indented(&conflicts)
            ));
        }
    } else {
        findings.push("Homebrew's \"bin\" was not found in your PATH.".to_owned());
    }

    if sbin_position.is_none() {
        let names = directory_names(&prefix_sbin)?;
        if names.iter().any(|name| name != ".keepme") {
            findings.push(format!(
                "Homebrew's \"sbin\" was not found in your PATH but you have installed\n\
                 formulae that put executables in {prefix_sbin}."
            ));
        }
    }

    Ok(findings)
}

fn directory_names(path: &Utf8Path) -> Result<BTreeSet<String>, OpError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(source) => return Err(OpError::io("inspect", path.to_path_buf(), source)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(BTreeSet::new());
    }
    let entries = fs::read_dir(path)
        .map_err(|source| OpError::io("read directory", path.to_path_buf(), source))?;
    let mut names = BTreeSet::new();
    for entry in entries {
        let entry =
            entry.map_err(|source| OpError::io("read directory", path.to_path_buf(), source))?;
        if let Some(name) = entry.file_name().to_str() {
            names.insert(name.to_owned());
        }
    }
    Ok(names)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootUsability {
    Usable,
    Missing,
    Invalid,
}

fn configured_root_usable(path: &Utf8Path) -> Result<RootUsability, OpError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Ok(RootUsability::Invalid)
        }
        Ok(_) => Ok(RootUsability::Usable),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(RootUsability::Missing),
        Err(source) => Err(OpError::io("inspect", path.to_path_buf(), source)),
    }
}

fn list_finding(heading: &str, paths: &[Utf8PathBuf], remediation: &str) -> String {
    format!("{heading}\n{}\n{remediation}", indented(paths))
}

fn indented<T: std::fmt::Display>(items: &[T]) -> String {
    items
        .iter()
        .map(|item| format!("  {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}
