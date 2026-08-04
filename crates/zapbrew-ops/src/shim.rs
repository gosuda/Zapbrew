//! Opt-in `<prefix>/bin/brew` shim.
//!
//! Installs or removes a symlink at `<prefix>/bin/brew` that points at the
//! zapbrew executable, plus the invocation-name seam ([`hint_program`]) used by
//! self-referential hints. Every filesystem inspection is no-follow and every
//! mutation is identity-safe: a foreign or dangling link is never overwritten
//! or deleted.

use std::fs;
use std::io;
use std::os::unix::fs::symlink;

use camino::{Utf8Path, Utf8PathBuf};

use crate::{Ctx, OpError};

/// Which shim mutation to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShimAction {
    Install,
    Remove,
}

/// Arguments for [`run`]; a named action, never a boolean selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Args {
    pub action: ShimAction,
}

/// Program name for a self-referential hint, derived from `argv0`.
///
/// Returns `brew` only when the basename, after one leading login `-`, is
/// exactly `brew`; otherwise `zapbrew`.
pub fn hint_program(argv0: &str) -> &'static str {
    let base = argv0.rsplit('/').next().unwrap_or(argv0);
    let base = base.strip_prefix('-').unwrap_or(base);
    if base == "brew" { "brew" } else { "zapbrew" }
}

/// Install or remove the brew shim, targeting the current executable.
pub fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let raw = std::env::current_exe().map_err(|source| OpError::Io {
        operation: "locate current executable",
        path: Utf8PathBuf::new(),
        source,
    })?;
    let exe = Utf8PathBuf::from_path_buf(raw).map_err(|path| OpError::Refusal {
        message: format!("executable path is not valid UTF-8: {}", path.display()),
    })?;
    run_with_exe(ctx, args, &exe)
}

/// [`run`] with an injected executable path; the hidden test seam.
pub(crate) fn run_with_exe(ctx: &Ctx, args: Args, exe: &Utf8Path) -> Result<(), OpError> {
    let exe = exe
        .canonicalize_utf8()
        .map_err(|source| OpError::io("canonicalize zapbrew executable", exe, source))?;
    let bin = prepare_bin(&ctx.env.prefix)?;
    let link = bin.join("brew");
    match args.action {
        ShimAction::Install => install(ctx, &link, &exe),
        ShimAction::Remove => remove(ctx, &link, &exe),
    }
}

/// Validate the prefix is a real directory and return a real `bin`, creating it
/// when absent. Symlinked prefix or bin is refused so the shim never escapes the
/// configured root.
fn prepare_bin(prefix: &Utf8Path) -> Result<Utf8PathBuf, OpError> {
    let meta = fs::symlink_metadata(prefix)
        .map_err(|source| OpError::io("inspect prefix", prefix, source))?;
    if meta.file_type().is_symlink() {
        return Err(refusal(format!(
            "Refusing to operate on symlinked prefix: {prefix}"
        )));
    }
    if !meta.is_dir() {
        return Err(refusal(format!("Prefix is not a directory: {prefix}")));
    }

    let bin = prefix.join("bin");
    match fs::symlink_metadata(&bin) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(refusal(format!(
                    "Refusing to operate on symlinked bin directory: {bin}"
                )));
            }
            if !meta.is_dir() {
                return Err(refusal(format!("bin exists but is not a directory: {bin}")));
            }
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(&bin)
                .map_err(|source| OpError::io("create bin directory", &bin, source))?;
        }
        Err(source) => return Err(OpError::io("inspect bin directory", &bin, source)),
    }
    Ok(bin)
}

fn install(ctx: &Ctx, link: &Utf8Path, exe: &Utf8Path) -> Result<(), OpError> {
    match fs::symlink_metadata(link) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            symlink(exe.as_std_path(), link.as_std_path())
                .map_err(|source| OpError::io("create brew shim", link, source))?;
            ctx.reporter
                .print(&format!("Installed brew shim: {link} -> {exe}"));
            Ok(())
        }
        Err(source) => Err(OpError::io("inspect brew shim", link, source)),
        Ok(meta) => {
            if !meta.file_type().is_symlink() {
                return Err(refusal(format!(
                    "Refusing to replace non-symlink at {link}"
                )));
            }
            let target = read_target(link)?;
            if matches_exe(link, &target, exe) {
                ctx.reporter
                    .print(&format!("brew shim already installed at {link}"));
                Ok(())
            } else {
                Err(refusal(format!(
                    "Refusing to replace existing shim: {link} -> {target}"
                )))
            }
        }
    }
}

fn remove(ctx: &Ctx, link: &Utf8Path, exe: &Utf8Path) -> Result<(), OpError> {
    match fs::symlink_metadata(link) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            ctx.reporter
                .print(&format!("No brew shim installed at {link}"));
            Ok(())
        }
        Err(source) => Err(OpError::io("inspect brew shim", link, source)),
        Ok(meta) => {
            if !meta.file_type().is_symlink() {
                return Err(refusal(format!("Refusing to remove non-symlink at {link}")));
            }
            let target = read_target(link)?;
            if matches_exe(link, &target, exe) {
                fs::remove_file(link)
                    .map_err(|source| OpError::io("remove brew shim", link, source))?;
                ctx.reporter.print(&format!("Removed brew shim: {link}"));
                Ok(())
            } else {
                Err(refusal(format!(
                    "Refusing to remove foreign brew shim: {link} -> {target}"
                )))
            }
        }
    }
}

/// Read a symlink's literal target without following it.
fn read_target(link: &Utf8Path) -> Result<Utf8PathBuf, OpError> {
    let raw = fs::read_link(link.as_std_path())
        .map_err(|source| OpError::io("read brew shim link", link, source))?;
    Utf8PathBuf::from_path_buf(raw).map_err(|path| {
        refusal(format!(
            "brew shim link target is not valid UTF-8: {}",
            path.display()
        ))
    })
}

/// Resolve a link's target (relative against its parent) and compare canonical
/// identity with the zapbrew executable. A dangling target never matches.
fn matches_exe(link: &Utf8Path, target: &Utf8Path, exe: &Utf8Path) -> bool {
    let resolved = if target.is_absolute() {
        target.to_path_buf()
    } else {
        match link.parent() {
            Some(parent) => parent.join(target),
            None => target.to_path_buf(),
        }
    };
    resolved
        .canonicalize_utf8()
        .map(|canonical| canonical == exe)
        .unwrap_or(false)
}

fn refusal(message: String) -> OpError {
    OpError::Refusal { message }
}
