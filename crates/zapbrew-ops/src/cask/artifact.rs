use std::os::unix::fs::{PermissionsExt, symlink};
use std::{fs, io};

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::Value;
use zapbrew_api::{Cask, CaskArtifact};
use zapbrew_prefix::CommandSpec;

use super::{
    artifact_target, checked_command, copy_entry, expand_path, first_source, path_exists,
    remove_entry, safe_relative, unsupported,
};
use crate::{Ctx, OpError};

/// A validated, ordered install artifact and its declared reverse action.
pub(super) enum Action {
    /// `app` / `suite`: move a staged directory into a destination.
    Move { source: String, target: Utf8PathBuf },
    /// `font` and plugin families, completions, `artifact`: copy staged path.
    Copy { source: String, target: Utf8PathBuf },
    /// `binary` / `manpage`: symlink a destination at the promoted keg path.
    Symlink {
        source: String,
        target: Utf8PathBuf,
        executable: bool,
    },
    /// `pkg`: invoke the injected installer on the staged package.
    Pkg { source: String },
}

/// The full validated install plan for one cask.
pub(super) struct Plan {
    pub actions: Vec<Action>,
    pub uninstall: Vec<Value>,
    pub zap: Vec<Value>,
}

/// Reverse of a single applied action, replayed on rollback.
pub(super) enum Reverse {
    Remove(Utf8PathBuf),
    /// Move a backed-up prior artifact from `backup` back to its `target`.
    Restore {
        backup: Utf8PathBuf,
        target: Utf8PathBuf,
    },
    Irreversible(String),
}

/// User-`~/Library` destination directory for a plugin family kind.
fn library_subdir(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "font" => "Fonts",
        "colorpicker" => "ColorPickers",
        "dictionary" => "Dictionaries",
        "input_method" => "Input Methods",
        "internet_plugin" => "Internet Plug-Ins",
        "keyboard_layout" => "Keyboard Layouts",
        "prefpane" => "PreferencePanes",
        "qlplugin" => "QuickLook",
        "mdimporter" => "Spotlight",
        "screen_saver" => "Screen Savers",
        "audio_unit_plugin" => "Audio/Plug-Ins/Components",
        "vst_plugin" => "Audio/Plug-Ins/VST",
        "vst3_plugin" => "Audio/Plug-Ins/VST3",
        _ => return None,
    })
}

/// Prefix-relative completion directory for a completion kind.
fn completion_dir(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "zsh_completion" => "share/zsh/site-functions",
        "bash_completion" => "etc/bash_completion.d",
        "fish_completion" => "share/fish/vendor_completions.d",
        _ => return None,
    })
}

/// Validate every artifact before any download or mutation (no partial preflight).
pub(super) fn plan(ctx: &Ctx, cask: &Cask, appdir: &Utf8Path) -> Result<Plan, OpError> {
    let token = &cask.token;
    let mut actions = Vec::new();
    let mut uninstall = Vec::new();
    let mut zap = Vec::new();

    for artifact in &cask.artifacts {
        let kind = artifact.kind.as_str();
        match kind {
            "uninstall" => uninstall.push(directive_array(artifact, token)?),
            "zap" => zap.push(directive_array(artifact, token)?),
            "app" | "suite" => {
                let source = source_of(artifact, token)?;
                let target = move_target(ctx, artifact, &source, appdir)?;
                actions.push(Action::Move { source, target });
            }
            "binary" | "manpage" => {
                let source = source_of(artifact, token)?;
                let target = symlink_target(ctx, kind, artifact, &source)?;
                actions.push(Action::Symlink {
                    source,
                    target,
                    executable: kind == "binary",
                });
            }
            "pkg" => actions.push(Action::Pkg {
                source: source_of(artifact, token)?,
            }),
            "artifact" => {
                let source = source_of(artifact, token)?;
                let raw = artifact_target(artifact).ok_or_else(|| unsupported(token, kind))?;
                let target = expand_path(ctx, &raw, appdir)?;
                actions.push(Action::Copy { source, target });
            }
            _ => {
                if let Some(subdir) = library_subdir(kind) {
                    let source = source_of(artifact, token)?;
                    let target = ctx
                        .env
                        .home
                        .join("Library")
                        .join(subdir)
                        .join(basename(artifact, &source, token)?);
                    actions.push(Action::Copy { source, target });
                } else if let Some(dir) = completion_dir(kind) {
                    let source = source_of(artifact, token)?;
                    let target = ctx
                        .env
                        .prefix
                        .join(dir)
                        .join(basename(artifact, &source, token)?);
                    actions.push(Action::Copy { source, target });
                } else {
                    return Err(unsupported(token, kind));
                }
            }
        }
    }

    Ok(Plan {
        actions,
        uninstall,
        zap,
    })
}

/// Apply the plan against `staging`, journalling each reverse action.
pub(super) fn apply(
    ctx: &Ctx,
    plan: &Plan,
    staging: &Utf8Path,
    final_dir: &Utf8Path,
    journal: &mut Vec<Reverse>,
) -> Result<(), OpError> {
    for action in &plan.actions {
        match action {
            Action::Move { source, target } => {
                let staged = staged_source(staging, source, &ctx.env.caskroom)?;
                ensure_absent(target)?;
                ensure_parent(target)?;
                journal.push(Reverse::Remove(target.clone()));
                move_path(&staged, target)?;
            }
            Action::Copy { source, target } => {
                let staged = staged_source(staging, source, &ctx.env.caskroom)?;
                ensure_absent(target)?;
                ensure_parent(target)?;
                journal.push(Reverse::Remove(target.clone()));
                copy_entry(&staged, target)?;
            }
            Action::Symlink {
                source,
                target,
                executable,
            } => {
                let final_source = staged_source(final_dir, source, &ctx.env.caskroom)?;
                if *executable {
                    let staged = staged_source(staging, source, &ctx.env.caskroom)?;
                    make_executable(&staged)?;
                }
                ensure_absent(target)?;
                ensure_parent(target)?;
                journal.push(Reverse::Remove(target.clone()));
                symlink(final_source.as_std_path(), target.as_std_path())
                    .map_err(|error| OpError::io("symlink", target, error))?;
            }
            Action::Pkg { source } => {
                let staged = staged_source(staging, source, &ctx.env.caskroom)?;
                journal.push(Reverse::Irreversible(format!(
                    "pkg installed from {staged} (not reversible)"
                )));
                checked_command(
                    ctx,
                    CommandSpec::new("/usr/sbin/installer")
                        .arg("-pkg")
                        .arg(staged.as_str())
                        .arg("-target")
                        .arg("/"),
                )?;
            }
        }
    }
    Ok(())
}

/// Move every reversible target of a prior install into `backup_root`, recording
/// a restore entry for each so a failed replacement can put them back.
///
/// `pkg` artifacts leave no reversible target, so they are skipped. Targets that
/// are already absent are left untouched.
pub(super) fn backup_old_targets(
    old_plan: &Plan,
    backup_root: &Utf8Path,
    journal: &mut Vec<Reverse>,
) -> Result<(), OpError> {
    for (index, action) in old_plan.actions.iter().enumerate() {
        let target = match action {
            Action::Move { target, .. }
            | Action::Copy { target, .. }
            | Action::Symlink { target, .. } => target,
            Action::Pkg { .. } => continue,
        };
        if !path_exists(target) {
            continue;
        }
        let backup = backup_root.join(index.to_string());
        ensure_parent(&backup)?;
        move_path(target, &backup)?;
        journal.push(Reverse::Restore {
            backup,
            target: target.clone(),
        });
    }
    Ok(())
}

/// Replay journalled reverse actions, returning any irreversible/failed leftovers.
pub(super) fn rollback(journal: &[Reverse]) -> Vec<String> {
    let mut leftovers = Vec::new();
    for reverse in journal.iter().rev() {
        match reverse {
            Reverse::Remove(path) => {
                if remove_entry(path).is_err() && path_exists(path) {
                    leftovers.push(path.to_string());
                }
            }
            Reverse::Restore { backup, target } => {
                if path_exists(target) && remove_entry(target).is_err() {
                    leftovers.push(target.to_string());
                } else if move_path(backup, target).is_err() && path_exists(backup) {
                    leftovers.push(backup.to_string());
                }
            }
            Reverse::Irreversible(description) => leftovers.push(description.clone()),
        }
    }
    leftovers
}

fn source_of(artifact: &CaskArtifact, token: &str) -> Result<String, OpError> {
    let source = first_source(artifact).ok_or_else(|| unsupported(token, &artifact.kind))?;
    if !safe_relative(&source) {
        return Err(OpError::Refusal {
            message: format!("Cask '{token}' has unsafe artifact source '{source}'."),
        });
    }
    Ok(source)
}

fn directive_array(artifact: &CaskArtifact, token: &str) -> Result<Value, OpError> {
    let array = artifact
        .value
        .get(&artifact.kind)
        .and_then(Value::as_array)
        .ok_or_else(|| unsupported(token, &artifact.kind))?;
    for directive in array {
        let object = directive
            .as_object()
            .ok_or_else(|| unsupported(token, &artifact.kind))?;
        for key in object.keys() {
            if !DIRECTIVE_KEYS.contains(&key.as_str()) {
                return Err(unsupported(token, &artifact.kind));
            }
        }
    }
    Ok(Value::Array(array.clone()))
}

/// Reversal directives recognized inside `uninstall` / `zap` stanzas.
pub(super) const DIRECTIVE_KEYS: &[&str] = &[
    "launchctl",
    "pkgutil",
    "delete",
    "trash",
    "rmdir",
    "quit",
    "signal",
];

fn move_target(
    ctx: &Ctx,
    artifact: &CaskArtifact,
    source: &str,
    appdir: &Utf8Path,
) -> Result<Utf8PathBuf, OpError> {
    if let Some(raw) = artifact_target(artifact) {
        return expand_path(ctx, &raw, appdir);
    }
    Ok(appdir.join(leaf(source)))
}

fn symlink_target(
    ctx: &Ctx,
    kind: &str,
    artifact: &CaskArtifact,
    source: &str,
) -> Result<Utf8PathBuf, OpError> {
    let name = basename(artifact, source, "cask")?;
    if kind == "manpage" {
        let section = man_section(source).ok_or_else(|| OpError::Refusal {
            message: format!("Cask manpage '{source}' has no section suffix."),
        })?;
        return Ok(ctx
            .env
            .prefix
            .join("share/man")
            .join(format!("man{section}"))
            .join(name));
    }
    Ok(ctx.env.prefix.join("bin").join(name))
}

fn basename(artifact: &CaskArtifact, source: &str, _token: &str) -> Result<String, OpError> {
    if let Some(raw) = artifact_target(artifact) {
        return Ok(leaf(&raw));
    }
    Ok(leaf(source))
}

fn leaf(path: &str) -> String {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .to_owned()
}

fn man_section(source: &str) -> Option<char> {
    let trimmed = source.strip_suffix(".gz").unwrap_or(source);
    trimmed
        .rsplit('.')
        .next()
        .filter(|section| section.len() == 1)
        .and_then(|section| section.chars().next())
        .filter(|c| c.is_ascii_digit() || *c == 'n' || *c == 'l')
}

fn staged_source(
    root: &Utf8Path,
    source: &str,
    caskroom: &Utf8Path,
) -> Result<Utf8PathBuf, OpError> {
    if !safe_relative(source) {
        return Err(OpError::Refusal {
            message: format!("Cask artifact source '{source}' is unsafe."),
        });
    }
    let joined = root.join(source);
    // Confine every staged/keg source under the Caskroom subtree.
    if !joined.starts_with(caskroom) {
        return Err(OpError::Refusal {
            message: format!("Cask artifact source '{source}' escapes the Caskroom."),
        });
    }
    Ok(joined)
}

fn ensure_absent(target: &Utf8Path) -> Result<(), OpError> {
    if path_exists(target) {
        return Err(OpError::Refusal {
            message: format!("It seems there is already an artifact at '{target}'."),
        });
    }
    Ok(())
}

fn ensure_parent(target: &Utf8Path) -> Result<(), OpError> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| OpError::io("create", parent, error))?;
    }
    Ok(())
}

fn move_path(source: &Utf8Path, target: &Utf8Path) -> Result<(), OpError> {
    match fs::rename(source, target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            copy_entry(source, target)?;
            remove_entry(source)
        }
        Err(error) => Err(OpError::io("move", target, error)),
    }
}

fn make_executable(path: &Utf8Path) -> Result<(), OpError> {
    let metadata = fs::metadata(path).map_err(|error| OpError::io("inspect", path, error))?;
    let mut perms = metadata.permissions();
    perms.set_mode(perms.mode() | 0o111);
    fs::set_permissions(path, perms).map_err(|error| OpError::io("chmod", path, error))
}
