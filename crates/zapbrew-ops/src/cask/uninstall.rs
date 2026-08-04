use std::collections::BTreeSet;
use std::fs;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::Value;
use zapbrew_prefix::CommandSpec;

use super::transaction::{installed_version_dirs, read_record};
use super::{
    acquire_locks, checked_command, confined_caskroom_child, expand_path, path_exists,
    remove_entry, require_macos, string_values,
};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub tokens: Vec<String>,
    pub zap: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    require_macos(ctx)?;
    let mut resolved = Vec::new();
    let mut canonical = BTreeSet::new();
    for token in &args.tokens {
        let token = resolve_installed(ctx, token)?;
        if canonical.insert(token.clone()) {
            resolved.push(token);
        }
    }
    let _locks = acquire_locks(ctx, &canonical)?;
    for token in &resolved {
        remove(ctx, token, args.zap)?;
    }
    Ok(())
}

/// Resolve a requested token to the canonical installed token.
///
/// Live catalog first, so an old-token alias maps to its canonical install; then
/// the raw token when its Caskroom directory exists, preserving
/// uninstall-from-record after a cask leaves the catalog.
fn resolve_installed(ctx: &Ctx, requested: &str) -> Result<String, OpError> {
    if let Some(cask) = ctx.casks.get(requested) {
        return Ok(cask.token.clone());
    }
    let dir = confined_caskroom_child(ctx, requested)?;
    if fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
        return Ok(requested.to_owned());
    }
    Err(OpError::Refusal {
        message: format!("Cask '{requested}' is unavailable."),
    })
}

/// Remove every installed version of `token` in one run. Every record is loaded
/// and every uninstall (and optional zap) directive is preflighted before any
/// command or filesystem mutation; a bad record or directive in any version
/// aborts the whole operation with nothing removed.
pub(super) fn remove(ctx: &Ctx, token: &str, zap: bool) -> Result<(), OpError> {
    let token_dir = ctx.env.caskroom.join(token);
    let versions = installed_version_dirs(&token_dir)?;
    if versions.is_empty() {
        return Err(not_installed(token));
    }
    let mut records = Vec::new();
    for version_dir in &versions {
        records.push(read_record(ctx, token, version_dir)?);
    }

    for record in &records {
        preflight_directives(ctx, &record.uninstall, &record.appdir())?;
        if zap {
            preflight_directives(ctx, &record.zap, &record.appdir())?;
        }
    }

    // Remove every deployed target across all versions, deduplicated, in reverse
    // application order (later versions reverse first).
    let mut seen = BTreeSet::new();
    let mut targets = Vec::new();
    for record in records.iter().rev() {
        for target in record.targets().collect::<Vec<_>>().into_iter().rev() {
            if seen.insert(target.clone()) {
                targets.push(target);
            }
        }
    }
    for target in &targets {
        remove_entry(target)?;
    }

    for (record, version_dir) in records.iter().zip(&versions) {
        run_directives(ctx, &record.uninstall, &record.appdir())?;
        if zap {
            run_directives(ctx, &record.zap, &record.appdir())?;
        }
        remove_entry(version_dir)?;
        let version = version_dir
            .file_name()
            .ok_or_else(|| OpError::InvalidState {
                reason: format!("installed cask version has no basename: {version_dir}"),
            })?;
        remove_version_receipts(&token_dir, version)?;
    }
    prune_empty(&token_dir)?;
    Ok(())
}

fn preflight_directives(ctx: &Ctx, groups: &[Value], appdir: &Utf8Path) -> Result<(), OpError> {
    for group in groups {
        let directives = group.as_array().ok_or_else(|| OpError::InvalidState {
            reason: "stored cask removal directive is not an array".to_owned(),
        })?;
        for directive in directives {
            let object = directive.as_object().ok_or_else(|| OpError::InvalidState {
                reason: "stored cask removal directive is not an object".to_owned(),
            })?;
            for (kind, value) in object {
                match kind.as_str() {
                    "launchctl" => {
                        let labels = string_values(value).ok_or_else(|| OpError::InvalidState {
                            reason: "stored launchctl directive has invalid shape".to_owned(),
                        })?;
                        for label in &labels {
                            validate_launchctl_label(label)?;
                        }
                    }
                    "pkgutil" | "quit" | "signal" => {
                        if kind == "signal" {
                            if !value.is_object() && !value.is_array() {
                                return Err(OpError::InvalidState {
                                    reason: "stored signal directive has invalid shape".to_owned(),
                                });
                            }
                        } else if string_values(value).is_none() {
                            return Err(OpError::InvalidState {
                                reason: format!("stored {kind} directive has invalid shape"),
                            });
                        }
                    }
                    "delete" | "trash" | "rmdir" => {
                        let paths = string_values(value).ok_or_else(|| OpError::InvalidState {
                            reason: format!("stored {kind} directive has invalid shape"),
                        })?;
                        for path in paths {
                            let expanded = expand_path(ctx, &path, appdir)?;
                            ensure_removal_root(ctx, &expanded, appdir)?;
                        }
                    }
                    _ => {
                        return Err(OpError::InvalidState {
                            reason: format!("stored cask removal kind '{kind}' is unsupported"),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

fn run_directives(ctx: &Ctx, groups: &[Value], appdir: &Utf8Path) -> Result<(), OpError> {
    for group in groups {
        let Some(directives) = group.as_array() else {
            continue;
        };
        for directive in directives {
            let Some(object) = directive.as_object() else {
                continue;
            };
            for (kind, value) in object {
                match kind.as_str() {
                    "launchctl" => {
                        for label in string_values(value).unwrap_or_default() {
                            let plist = launch_agent_plist(ctx, &label)?;
                            checked_command(
                                ctx,
                                CommandSpec::new("/bin/launchctl")
                                    .arg("unload")
                                    .arg(plist.as_str()),
                            )?;
                            remove_entry(&plist)?;
                        }
                    }
                    "pkgutil" => {
                        for package in string_values(value).unwrap_or_default() {
                            checked_command(
                                ctx,
                                CommandSpec::new("/usr/sbin/pkgutil")
                                    .arg("--forget")
                                    .arg(package),
                            )?;
                        }
                    }
                    "delete" | "trash" => {
                        for path in string_values(value).unwrap_or_default() {
                            remove_entry(&expand_path(ctx, &path, appdir)?)?;
                        }
                    }
                    "rmdir" => {
                        for path in string_values(value).unwrap_or_default() {
                            let path = expand_path(ctx, &path, appdir)?;
                            match fs::remove_dir(&path) {
                                Ok(()) => {}
                                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                                Err(source) => return Err(OpError::io("rmdir", &path, source)),
                            }
                        }
                    }
                    "quit" | "signal" => ctx
                        .reporter
                        .opoo(&format!("Skipping unsupported cask {kind} directive.")),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn ensure_removal_root(ctx: &Ctx, path: &Utf8Path, appdir: &Utf8Path) -> Result<(), OpError> {
    if path.starts_with(&ctx.env.home)
        || path.starts_with(&ctx.env.prefix)
        || path.starts_with(appdir)
    {
        Ok(())
    } else {
        Err(OpError::Refusal {
            message: format!("Cask removal path '{path}' is outside approved roots."),
        })
    }
}

/// Validate a stored `launchctl` label before it builds a plist path or runs a
/// command. A launchd label is a nonempty string of ASCII letters, digits, `.`,
/// `_`, and `-`; any slash, separator, parent component, control byte, or other
/// character is invalid stored record data.
fn validate_launchctl_label(label: &str) -> Result<(), OpError> {
    let valid = !label.is_empty()
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(OpError::InvalidState {
            reason: format!("stored launchctl label {label:?} is invalid"),
        })
    }
}

/// Build a LaunchAgents plist path from a validated launchd label. The label is
/// validated first, so the returned path is always a single filename confined to
/// `~/Library/LaunchAgents`.
fn launch_agent_plist(ctx: &Ctx, label: &str) -> Result<Utf8PathBuf, OpError> {
    validate_launchctl_label(label)?;
    Ok(ctx
        .env
        .home
        .join("Library/LaunchAgents")
        .join(format!("{label}.plist")))
}

fn remove_version_receipts(token_dir: &Utf8Path, version: &str) -> Result<(), OpError> {
    let path = token_dir.join(".metadata").join(version);
    remove_entry(&path)?;
    let metadata = token_dir.join(".metadata");
    if metadata.is_dir()
        && fs::read_dir(&metadata).is_ok_and(|mut entries| entries.next().is_none())
    {
        fs::remove_dir(&metadata).map_err(|source| OpError::io("remove", &metadata, source))?;
    }
    Ok(())
}

fn prune_empty(token_dir: &Utf8Path) -> Result<(), OpError> {
    if path_exists(token_dir)
        && fs::read_dir(token_dir).is_ok_and(|mut entries| entries.next().is_none())
    {
        fs::remove_dir(token_dir).map_err(|source| OpError::io("remove", token_dir, source))?;
    }
    Ok(())
}

fn not_installed(token: &str) -> OpError {
    OpError::Refusal {
        message: format!("Cask '{token}' is not installed."),
    }
}
