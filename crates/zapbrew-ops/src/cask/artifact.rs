use std::os::unix::fs::{PermissionsExt, symlink};
use std::{fs, io};

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use serde_json::Value;
use zapbrew_api::{CASK_ARTIFACT_KINDS, Cask, CaskArtifact};
use zapbrew_prefix::CommandSpec;
use zapbrew_types::BottleTag;

use super::{
    artifact_target, checked_command, confined_caskroom_dir, confined_caskroom_path, copy_entry,
    expand_path, first_source, one_normal_component, path_exists, remove_entry, safe_lexical,
    safe_relative, unsupported,
};
use crate::{Ctx, OpError};

/// A validated, ordered install artifact and its declared reverse action.
pub(super) enum Action {
    /// `app` / `suite`: move a staged directory into a destination.
    Move { source: String, target: Utf8PathBuf },
    /// `font` and plugin families, completions, `artifact`: copy staged path.
    Copy { source: String, target: Utf8PathBuf },
    /// `binary` / `manpage` / `appimage`: symlink a destination.
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
    /// Move a whole backed-up tree (old version dir or metadata receipt tree)
    /// from `backup` back to its `original` location.
    RestoreDir {
        backup: Utf8PathBuf,
        original: Utf8PathBuf,
    },
    Irreversible(String),
}

/// Kinds that are permanently refused as cask-supplied execution.
const EXECUTION_KINDS: &[&str] = &[
    "preflight",
    "postflight",
    "uninstall_preflight",
    "uninstall_postflight",
    "preflight_steps",
    "postflight_steps",
    "uninstall_preflight_steps",
    "uninstall_postflight_steps",
];

/// Kinds that only make sense on macOS and are refused on Linux.
const MACOS_ONLY_KINDS: &[&str] = &[
    "app",
    "suite",
    "pkg",
    "service",
    "colorpicker",
    "dictionary",
    "input_method",
    "internet_plugin",
    "keyboard_layout",
    "prefpane",
    "qlplugin",
    "mdimporter",
    "screen_saver",
    "audio_unit_plugin",
    "vst_plugin",
    "vst3_plugin",
];
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
    let is_linux = !matches!(ctx.env.bottle_tag, BottleTag::MacOs { .. });
    if is_linux && cask.depends_on.macos.is_some() {
        return Err(requires_macos(token));
    }

    let mut actions = Vec::new();
    let mut uninstall = Vec::new();
    let mut zap = Vec::new();
    let mut stage_only = false;

    for artifact in &cask.artifacts {
        let kind = classify_artifact(artifact, token)?;
        validate_artifact(artifact, token, kind)?;

        if kind == "stage_only" {
            stage_only = true;
            continue;
        }

        if kind == "uninstall" {
            uninstall.push(directive_array(artifact, token)?);
            continue;
        }

        if kind == "zap" {
            zap.push(directive_array(artifact, token)?);
            continue;
        }

        // Linux macOS-only and cask-supplied-execution refusal before any effect.
        if let Some(err) = refuse_kind(token, kind, is_linux) {
            return Err(err);
        }

        // `installer` carries a nested manual/script directive and needs the
        // artifact value, so it is refused before the platform-agnostic match.
        if kind == "installer" {
            return Err(installer_refusal(token, artifact, is_linux));
        }

        match kind {
            "binary" | "manpage" | "appimage" => {
                let source = source_of(artifact, token)?;
                let target = symlink_target(ctx, kind, artifact, &source, token)?;
                ensure_target_root(ctx, &target, appdir)?;
                actions.push(Action::Symlink {
                    source,
                    target,
                    executable: kind == "binary" || kind == "appimage",
                });
            }
            "artifact" => {
                let source = source_of(artifact, token)?;
                let raw = artifact_target(artifact).ok_or_else(|| unsupported(token, kind))?;
                let target = expand_path(ctx, &raw, appdir)?;
                ensure_target_root(ctx, &target, appdir)?;
                actions.push(Action::Copy { source, target });
            }
            "font" => {
                let source = source_of(artifact, token)?;
                let target = ctx
                    .env
                    .home
                    .join("Library")
                    .join("Fonts")
                    .join(basename(artifact, &source, token)?);
                ensure_target_root(ctx, &target, appdir)?;
                actions.push(Action::Copy { source, target });
            }
            "zsh_completion" | "bash_completion" | "fish_completion" => {
                let source = source_of(artifact, token)?;
                let dir = completion_dir(kind).ok_or_else(|| unsupported(token, kind))?;
                let target = ctx
                    .env
                    .prefix
                    .join(dir)
                    .join(basename(artifact, &source, token)?);
                ensure_target_root(ctx, &target, appdir)?;
                actions.push(Action::Copy { source, target });
            }
            "app" | "suite" if !is_linux => {
                let source = source_of(artifact, token)?;
                let target = move_target(ctx, artifact, &source, appdir, token)?;
                ensure_target_root(ctx, &target, appdir)?;
                actions.push(Action::Move { source, target });
            }
            "pkg" if !is_linux => {
                actions.push(Action::Pkg {
                    source: source_of(artifact, token)?,
                });
            }
            _ if !is_linux && library_subdir(kind).is_some() => {
                let source = source_of(artifact, token)?;
                let subdir = library_subdir(kind).ok_or_else(|| unsupported(token, kind))?;
                let target = ctx
                    .env
                    .home
                    .join("Library")
                    .join(subdir)
                    .join(basename(artifact, &source, token)?);
                ensure_target_root(ctx, &target, appdir)?;
                actions.push(Action::Copy { source, target });
            }
            _ => return Err(unsupported(token, kind)),
        }
    }

    if stage_only && !actions.is_empty() {
        return Err(OpError::Refusal {
            message: format!(
                "Cask '{token}' declares 'stage_only' with additional deployable artifacts"
            ),
        });
    }

    Ok(Plan {
        actions,
        uninstall,
        zap,
    })
}

fn requires_macos(token: &str) -> OpError {
    OpError::Refusal {
        message: format!("{token}: This cask requires macOS."),
    }
}

/// Return the single recognized directive kind for this artifact, or refuse
/// unknown, malformed, and multi-directive artifacts.
fn classify_artifact(artifact: &CaskArtifact, token: &str) -> Result<&'static str, OpError> {
    let obj = artifact
        .value
        .as_object()
        .ok_or_else(|| unsupported(token, &artifact.kind))?;
    let recognized: Vec<&'static str> = CASK_ARTIFACT_KINDS
        .iter()
        .copied()
        .filter(|&k| obj.contains_key(k))
        .collect();
    match recognized.as_slice() {
        [] => Err(unsupported(token, &artifact.kind)),
        [one] => Ok(one),
        _ => Err(OpError::Refusal {
            message: format!(
                "Cask '{token}' artifact declares multiple directives; exactly one is required."
            ),
        }),
    }
}

/// Allowed nested option keys for a `script:` installer directive.
const SCRIPT_OPTIONS: &[&str] = &[
    "executable",
    "args",
    "env",
    "sudo",
    "must_succeed",
    "print_stdout",
    "print_stderr",
    "input",
];

/// Refuse an artifact with an unrecognized top-level or nested option key.
fn unknown_artifact_key(token: &str, key: &str) -> OpError {
    OpError::Refusal {
        message: format!("Cask '{token}' artifact has unknown key '{key}'."),
    }
}

fn unknown_artifact_option(token: &str, kind: &str, key: &str) -> OpError {
    OpError::Refusal {
        message: format!("Cask '{token}' artifact '{kind}' has unknown option '{key}'."),
    }
}

/// True when `key` is a recognized top-level key for an artifact of `kind`.
fn is_top_key_allowed(kind: &str, key: &str) -> bool {
    if key == kind {
        return true;
    }
    matches!(
        kind,
        "app"
            | "suite"
            | "artifact"
            | "binary"
            | "manpage"
            | "appimage"
            | "font"
            | "zsh_completion"
            | "bash_completion"
            | "fish_completion"
            | "audio_unit_plugin"
            | "colorpicker"
            | "dictionary"
            | "input_method"
            | "internet_plugin"
            | "keyboard_layout"
            | "mdimporter"
            | "prefpane"
            | "qlplugin"
            | "screen_saver"
            | "vst_plugin"
            | "vst3_plugin"
    ) && key == "target"
}

/// Nested option-object keys allowed per artifact kind. The second array element
/// of a relocated artifact may only contain `target`; `pkg` does not use a nested
/// options object; `uninstall`/`zap` use their own directive validation.
fn allowed_nested_keys(kind: &str) -> &'static [&'static str] {
    match kind {
        "installer" | "pkg" | "uninstall" | "zap" | "stage_only" => &[],
        _ => &["target"],
    }
}

/// Validate the complete artifact schema: every top-level key is declared for the
/// kind, and every nested option object contains only permitted keys. This keeps
/// ignored keys from crossing the planner.
fn validate_artifact(artifact: &CaskArtifact, token: &str, kind: &str) -> Result<(), OpError> {
    let obj = artifact
        .value
        .as_object()
        .ok_or_else(|| unsupported(token, &artifact.kind))?;

    for key in obj.keys() {
        if !is_top_key_allowed(kind, key.as_str()) {
            return Err(unknown_artifact_key(token, key));
        }
    }

    match kind {
        "stage_only" => validate_stage_only(artifact, token),
        "uninstall" | "zap" => validate_directive_array(artifact, token),
        "installer" => validate_installer_value(artifact, token),
        "pkg" => validate_pkg_value(artifact, token),
        _ => validate_relocated_value(artifact, token, kind),
    }
}
/// `uninstall`/`zap` values are arrays of directive objects; the existing
/// `directive_array` already validates each object's keys.
fn validate_directive_array(artifact: &CaskArtifact, token: &str) -> Result<(), OpError> {
    let _ = directive_array(artifact, token)?;
    Ok(())
}

fn validate_installer_value(artifact: &CaskArtifact, token: &str) -> Result<(), OpError> {
    let array = artifact
        .value
        .get("installer")
        .and_then(Value::as_array)
        .ok_or_else(|| unsupported(token, "installer"))?;
    if array.len() != 1 {
        return Err(unsupported(token, "installer"));
    }
    let obj = array[0]
        .as_object()
        .ok_or_else(|| unsupported(token, "installer"))?;
    if obj.len() != 1 {
        return Err(unsupported(token, "installer"));
    }
    let (key, value) = obj
        .iter()
        .next()
        .ok_or_else(|| unsupported(token, "installer"))?;
    match key.as_str() {
        "manual" if value.is_string() => Ok(()),
        "script" => validate_script_options(token, value),
        _ => Err(unsupported(token, "installer")),
    }
}

fn validate_script_options(token: &str, value: &Value) -> Result<(), OpError> {
    match value {
        Value::String(_) => Ok(()),
        Value::Object(map) => {
            for key in map.keys() {
                if !SCRIPT_OPTIONS.contains(&key.as_str()) {
                    return Err(unknown_artifact_option(token, "installer", key));
                }
            }
            Ok(())
        }
        _ => Err(unsupported(token, "installer")),
    }
}

fn validate_pkg_value(artifact: &CaskArtifact, token: &str) -> Result<(), OpError> {
    let value = artifact
        .value
        .get("pkg")
        .ok_or_else(|| unsupported(token, "pkg"))?;
    let array = match value.as_array() {
        Some(array) => array,
        None => {
            if value.is_string() {
                return Ok(());
            }
            return Err(unsupported(token, "pkg"));
        }
    };
    let mut iter = array.iter();
    let first = iter
        .next()
        .and_then(Value::as_str)
        .ok_or_else(|| unsupported(token, "pkg"))?;
    if first.is_empty() {
        return Err(unsupported(token, "pkg"));
    }
    if iter.next().is_some() {
        return Err(unsupported(token, "pkg"));
    }
    Ok(())
}

fn validate_relocated_value(
    artifact: &CaskArtifact,
    token: &str,
    kind: &str,
) -> Result<(), OpError> {
    let value = artifact
        .value
        .get(kind)
        .ok_or_else(|| unsupported(token, kind))?;
    let top_target = match artifact.value.get("target") {
        Some(Value::String(target)) if !target.is_empty() => Some(target.as_str()),
        Some(_) => return Err(unsupported(token, kind)),
        None => None,
    };
    let array = match value.as_array() {
        Some(array) => array,
        None => {
            if value.as_str().is_some_and(|source| !source.is_empty()) {
                return Ok(());
            }
            return Err(unsupported(token, kind));
        }
    };
    if array.is_empty() {
        return Err(unsupported(token, kind));
    }
    let first = array[0].as_str().ok_or_else(|| unsupported(token, kind))?;
    if first.is_empty() {
        return Err(unsupported(token, kind));
    }
    if let Some(extra) = array.get(1) {
        let map = extra.as_object().ok_or_else(|| unsupported(token, kind))?;
        for key in map.keys() {
            if !allowed_nested_keys(kind).contains(&key.as_str()) {
                return Err(unknown_artifact_option(token, kind, key));
            }
        }
        if let Some(target) = map.get("target")
            && (!target.as_str().is_some_and(|target| !target.is_empty()) || top_target.is_some())
        {
            return Err(unsupported(token, kind));
        }
        if array.len() > 2 {
            return Err(unsupported(token, kind));
        }
    } else if array.len() > 1 {
        return Err(unsupported(token, kind));
    }
    Ok(())
}

/// `stage_only` must be exactly `[true]` (JSON boolean).
fn validate_stage_only(artifact: &CaskArtifact, token: &str) -> Result<(), OpError> {
    let value = artifact
        .value
        .get("stage_only")
        .ok_or_else(|| unsupported(token, "stage_only"))?;
    let array = value
        .as_array()
        .ok_or_else(|| unsupported(token, "stage_only"))?;
    if array.len() != 1 || array[0].as_bool() != Some(true) {
        return Err(unsupported(token, "stage_only"));
    }
    Ok(())
}

/// Refuse Linux macOS-only artifacts and all cask-supplied execution.  `None`
/// means the kind is deployable on this platform.
fn refuse_kind(token: &str, kind: &'static str, is_linux: bool) -> Option<OpError> {
    if is_linux && MACOS_ONLY_KINDS.contains(&kind) {
        return Some(requires_macos(token));
    }
    if !is_linux && kind == "appimage" {
        return Some(unsupported(token, kind));
    }
    if EXECUTION_KINDS.contains(&kind) {
        return Some(unsupported(token, kind));
    }
    None
}

/// `installer` must carry exactly one nested `manual` or `script` directive.
fn installer_refusal(token: &str, artifact: &CaskArtifact, is_linux: bool) -> OpError {
    let array = match artifact.value.get("installer").and_then(Value::as_array) {
        Some(array) => array,
        None => return unsupported(token, "installer"),
    };
    if array.len() != 1 {
        return unsupported(token, "installer");
    }
    let obj = match array[0].as_object() {
        Some(obj) => obj,
        None => return unsupported(token, "installer"),
    };
    if obj.len() != 1 {
        return unsupported(token, "installer");
    }
    if is_linux && obj.contains_key("manual") {
        return requires_macos(token);
    }
    if obj.contains_key("script") {
        return unsupported(token, "installer");
    }
    unsupported(token, "installer")
}
/// Apply the plan against `staging`, journalling each reverse action.
pub(super) fn apply(
    ctx: &Ctx,
    plan: &Plan,
    staging: &Utf8Path,
    final_dir: &Utf8Path,
    appdir: &Utf8Path,
    journal: &mut Vec<Reverse>,
) -> Result<(), OpError> {
    for action in &plan.actions {
        match action {
            Action::Move { source, target } => {
                let staged = staged_source(staging, source, &ctx.env.caskroom)?;
                confined_target_physical(ctx, target, &[appdir])?;
                ensure_absent(target)?;
                ensure_parent_journaled(target, journal)?;
                journal.push(Reverse::Remove(target.clone()));
                move_path(&staged, target)?;
            }
            Action::Copy { source, target } => {
                let staged = staged_source(staging, source, &ctx.env.caskroom)?;
                confined_target_physical(ctx, target, &[appdir])?;
                ensure_absent(target)?;
                ensure_parent_journaled(target, journal)?;
                journal.push(Reverse::Remove(target.clone()));
                copy_entry(&staged, target)?;
            }
            Action::Symlink {
                source,
                target,
                executable,
            } => {
                // No-follow-validate the extracted staging source even when we
                // only symlink to its future promoted keg path.
                let staged = staged_source(staging, source, &ctx.env.caskroom)?;
                if *executable {
                    make_executable(&staged)?;
                }
                // The promoted version directory does not exist until staging is
                // atomically renamed into place, so confine the future symlink
                // source lexically only.
                let final_source = confined_source_path(final_dir, source, &ctx.env.caskroom)?;
                confined_target_physical(ctx, target, &[appdir])?;
                ensure_absent(target)?;
                ensure_parent_journaled(target, journal)?;
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

/// Move every prior deployed target into `backup_root`, recording a restore
/// entry for each so a failed replacement can put it back. `targets` comes from
/// the persisted install records (already deduplicated by the caller), so
/// replacement operates on what was actually deployed. Targets that are already
/// absent are left untouched.
pub(super) fn backup_targets<'a>(
    ctx: &Ctx,
    targets: impl Iterator<Item = &'a Utf8Path>,
    backup_root: &Utf8Path,
    appdirs: &[&Utf8Path],
    journal: &mut Vec<Reverse>,
) -> Result<(), OpError> {
    for (index, target) in targets.enumerate() {
        if !path_exists(target) {
            continue;
        }
        confined_target_physical(ctx, target, appdirs)?;
        let backup = backup_root.join(format!("target-{index}"));
        confined_caskroom_path(ctx, &backup)?;
        ensure_parent(&backup)?;
        backup_target(target, &backup)?;
        journal.push(Reverse::Restore {
            backup,
            target: target.to_path_buf(),
        });
    }
    Ok(())
}

/// Remove every drained backup tree after a committed replacement. All backups
/// are siblings under one `.staging` root; cleanup is best-effort and failures
/// are ignored because leftovers are inert.
pub(super) fn drain_backups(journal: &[Reverse]) {
    for reverse in journal {
        let backup = match reverse {
            Reverse::Restore { backup, .. } | Reverse::RestoreDir { backup, .. } => backup,
            Reverse::Remove(_) | Reverse::Irreversible(_) => continue,
        };
        let _ = remove_entry(backup);
    }
}
/// Replay journalled reverse actions, returning any irreversible/failed leftovers.
pub(super) fn rollback(ctx: &Ctx, journal: &[Reverse], appdirs: &[&Utf8Path]) -> Vec<String> {
    let mut leftovers = Vec::new();
    for reverse in journal.iter().rev() {
        match reverse {
            Reverse::Remove(path) => {
                if confined_target_physical(ctx, path, appdirs).is_err() {
                    leftovers.push(path.to_string());
                    continue;
                }
                if remove_entry(path).is_err() && path_exists(path) {
                    leftovers.push(path.to_string());
                }
            }
            Reverse::Restore { backup, target } => {
                // The backup leaf can itself be a symlink (e.g. a binary/manpage
                // deployed target), so confine its parent directory instead.
                let Some(backup_parent) = backup.parent() else {
                    leftovers.push(target.to_string());
                    continue;
                };
                if confined_caskroom_dir(ctx, backup_parent).is_err()
                    || confined_target_physical(ctx, target, appdirs).is_err()
                {
                    leftovers.push(target.to_string());
                    continue;
                }
                if path_exists(target) && remove_entry(target).is_err() {
                    leftovers.push(target.to_string());
                } else if move_path(backup, target).is_err() && path_exists(backup) {
                    leftovers.push(backup.to_string());
                }
            }
            Reverse::RestoreDir { backup, original } => {
                if confined_caskroom_dir(ctx, backup).is_err()
                    || confined_caskroom_dir(ctx, original).is_err()
                {
                    leftovers.push(original.to_string());
                    continue;
                }
                if path_exists(original) && remove_entry(original).is_err() {
                    leftovers.push(original.to_string());
                } else if move_path(backup, original).is_err() && path_exists(backup) {
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
    let value = artifact
        .value
        .get(&artifact.kind)
        .ok_or_else(|| unsupported(token, &artifact.kind))?;
    if !directives_valid(value) {
        return Err(unsupported(token, &artifact.kind));
    }
    Ok(value.clone())
}

pub(super) fn directives_valid(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|array| array.iter().all(directive_valid))
}

fn directive_valid(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        !object.is_empty()
            && object.iter().all(|(kind, value)| {
                DIRECTIVE_KEYS.contains(&kind.as_str()) && directive_value_valid(kind, value)
            })
    })
}

pub(super) fn directive_value_valid(kind: &str, value: &Value) -> bool {
    match kind {
        "launchctl" | "pkgutil" | "delete" | "trash" | "rmdir" | "quit" => nonempty_strings(value),
        "signal" => signal_value_valid(value),
        _ => false,
    }
}

fn nonempty_strings(value: &Value) -> bool {
    match value {
        Value::String(value) => !value.is_empty(),
        Value::Array(values) => {
            !values.is_empty()
                && values
                    .iter()
                    .all(|value| value.as_str().is_some_and(|value| !value.is_empty()))
        }
        _ => false,
    }
}

fn signal_pair(value: &Value) -> bool {
    value.as_array().is_some_and(|pair| {
        pair.len() == 2
            && pair
                .iter()
                .all(|value| value.as_str().is_some_and(|value| !value.is_empty()))
    })
}

fn signal_value_valid(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            !map.is_empty()
                && map.iter().all(|(signal, target)| {
                    !signal.is_empty() && target.as_str().is_some_and(|target| !target.is_empty())
                })
        }
        Value::Array(values) => {
            signal_pair(value) || (!values.is_empty() && values.iter().all(signal_pair))
        }
        _ => false,
    }
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

/// Shared no-follow confinement for any path that will become a sink for cask
/// effects (deploy target, force-replace backup, or persisted record target).
/// The path must be absolute, lexically safe, and start under one approved root
/// (home, prefix, or an approved appdir).  Every existing ancestor up to that
/// root is inspected with `symlink_metadata`; a symlink, non-directory ancestor,
/// or I/O error blocks the operation before we create, rename, copy, or remove
/// through it.
pub(super) fn confined_target_physical(
    ctx: &Ctx,
    target: &Utf8Path,
    appdirs: &[&Utf8Path],
) -> Result<(), OpError> {
    if !target.is_absolute() || !safe_lexical(target.as_std_path()) {
        return Err(OpError::Refusal {
            message: format!("Cask target '{target}' is outside approved roots."),
        });
    }
    if target == ctx.env.home
        || target == ctx.env.prefix
        || target == Utf8Path::new(crate::cask::DEFAULT_APPDIR)
        || appdirs.contains(&target)
    {
        return Err(OpError::Refusal {
            message: format!("Cask target '{target}' must be below an approved root."),
        });
    }
    if target.starts_with(&ctx.env.caskroom) {
        return Err(OpError::Refusal {
            message: format!("Cask target '{target}' enters the managed Caskroom."),
        });
    }
    let authorized = target.starts_with(&ctx.env.home)
        || target.starts_with(&ctx.env.prefix)
        || appdirs.iter().any(|appdir| target.starts_with(appdir));
    if !authorized {
        return Err(OpError::Refusal {
            message: format!("Cask target '{target}' is outside approved roots."),
        });
    }
    let fixed_applications = Utf8Path::new(crate::cask::DEFAULT_APPDIR);
    let root = [
        fixed_applications,
        ctx.env.home.as_path(),
        ctx.env.prefix.as_path(),
    ]
    .into_iter()
    .filter(|root| target.starts_with(root))
    .max_by_key(|root| root.components().count())
    .ok_or_else(|| OpError::Refusal {
        message: format!("Cask target '{target}' is outside approved physical roots."),
    })?;

    let mut current = target;
    while current != root {
        current = current.parent().unwrap_or(root);
        let meta = match fs::symlink_metadata(current) {
            Ok(meta) => meta,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(OpError::io("inspect", current, source)),
        };
        if meta.file_type().is_symlink() {
            return Err(OpError::Refusal {
                message: format!("Cask target '{target}' resolves through symlink '{current}'."),
            });
        }
        if !meta.is_dir() {
            return Err(OpError::Refusal {
                message: format!("Cask target '{target}' has non-directory ancestor '{current}'."),
            });
        }
    }
    Ok(())
}
/// Refuse a deploy target outside the approved roots, mirroring
/// `InstallRecord::validate` on the removal path so an installed cask can
/// always be uninstalled and replaced.
fn ensure_target_root(ctx: &Ctx, target: &Utf8Path, appdir: &Utf8Path) -> Result<(), OpError> {
    confined_target_physical(ctx, target, &[appdir])
}

fn move_target(
    ctx: &Ctx,
    artifact: &CaskArtifact,
    source: &str,
    appdir: &Utf8Path,
    token: &str,
) -> Result<Utf8PathBuf, OpError> {
    if let Some(raw) = artifact_target(artifact) {
        return expand_path(ctx, &raw, appdir);
    }
    let name = leaf(source);
    if !one_normal_component(&name) {
        return Err(OpError::Refusal {
            message: format!("Cask '{token}' has unsafe app name '{name}'."),
        });
    }
    Ok(appdir.join(name))
}

fn symlink_target(
    ctx: &Ctx,
    kind: &str,
    artifact: &CaskArtifact,
    source: &str,
    token: &str,
) -> Result<Utf8PathBuf, OpError> {
    let name = basename(artifact, source, token)?;
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
    if kind == "appimage" {
        return Ok(ctx.env.home.join("Applications").join(name));
    }
    Ok(ctx.env.prefix.join("bin").join(name))
}

fn basename(artifact: &CaskArtifact, source: &str, token: &str) -> Result<String, OpError> {
    let raw = if let Some(raw) = artifact_target(artifact) {
        raw
    } else {
        source.to_owned()
    };
    let name = leaf(&raw);
    if !one_normal_component(&name) {
        return Err(OpError::Refusal {
            message: format!(
                "Cask '{token}' has unsafe {kind} name '{name}'.",
                kind = artifact.kind
            ),
        });
    }
    Ok(name)
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
    let joined = confined_source_path(root, source, caskroom)?;
    // The staging root itself must be a real directory before we walk into it.
    let root_meta =
        fs::symlink_metadata(root).map_err(|error| OpError::io("inspect", root, error))?;
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return Err(OpError::Refusal {
            message: format!("Cask staging root '{root}' is not a real directory."),
        });
    }
    let mut components: Vec<&str> = Vec::new();
    for component in Utf8Path::new(source).components() {
        match component {
            Utf8Component::Normal(name) => components.push(name),
            Utf8Component::CurDir => {}
            _ => {
                return Err(OpError::Refusal {
                    message: format!("Cask artifact source '{source}' is unsafe."),
                });
            }
        }
    }
    if components.is_empty() {
        return Err(OpError::Refusal {
            message: format!("Cask artifact source '{source}' is unsafe."),
        });
    }
    // Inspect every component with no-follow metadata: intermediates must be real
    // directories and the final source must exist and not be a symlink. Reject
    // any symlink/non-directory component without stat-ing, renaming, or copying
    // through it.
    let last = components.len() - 1;
    let mut current = root.to_path_buf();
    for (index, name) in components.iter().enumerate() {
        current.push(name);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| OpError::io("inspect", &current, error))?;
        if metadata.file_type().is_symlink() {
            return Err(OpError::Refusal {
                message: format!(
                    "Cask artifact source '{source}' resolves through symlink '{current}'."
                ),
            });
        }
        if index != last && !metadata.is_dir() {
            return Err(OpError::Refusal {
                message: format!(
                    "Cask artifact source '{source}' has a non-directory component '{current}'."
                ),
            });
        }
    }
    Ok(joined)
}

/// Confine a declared artifact source lexically: reject non-relative or
/// traversal paths and any join that would escape the Caskroom subtree.
fn confined_source_path(
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

fn ensure_parent_journaled(target: &Utf8Path, journal: &mut Vec<Reverse>) -> Result<(), OpError> {
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    let mut current = parent;
    let mut created_root = None;
    loop {
        match fs::symlink_metadata(current) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                created_root = Some(current.to_path_buf());
                let Some(next) = current.parent() else {
                    break;
                };
                current = next;
            }
            Err(error) => return Err(OpError::io("inspect", current, error)),
        }
    }
    if let Some(root) = created_root {
        journal.push(Reverse::Remove(root));
    }
    fs::create_dir_all(parent).map_err(|error| OpError::io("create", parent, error))
}
fn backup_target(source: &Utf8Path, backup: &Utf8Path) -> Result<(), OpError> {
    match fs::rename(source, backup) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
            if let Err(original) = copy_entry(source, backup) {
                if remove_entry(backup).is_err() && path_exists(backup) {
                    return Err(OpError::RollbackIncomplete {
                        original: Box::new(original),
                        leftovers: backup.to_string(),
                    });
                }
                return Err(original);
            }
            if let Err(original) = remove_entry(source) {
                return Err(OpError::RollbackIncomplete {
                    original: Box::new(original),
                    leftovers: backup.to_string(),
                });
            }
            Ok(())
        }
        Err(error) => Err(OpError::io("move", backup, error)),
    }
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
