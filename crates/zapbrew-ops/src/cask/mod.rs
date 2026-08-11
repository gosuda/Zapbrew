use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use serde_json::Value;
use zapbrew_api::Cask;
use zapbrew_prefix::{CommandSpec, LockGuard};
use zapbrew_types::BottleTag;

use crate::{Ctx, OpError};

mod archive;
mod artifact;
pub mod install;
pub mod list;
mod transaction;
pub mod uninstall;

static UNIQUE_ID: AtomicU64 = AtomicU64::new(1);

fn require_macos(ctx: &Ctx) -> Result<(), OpError> {
    if matches!(ctx.env.bottle_tag, BottleTag::MacOs { .. }) {
        Ok(())
    } else {
        Err(OpError::Refusal {
            message: "Casks are not supported on Linux.".to_owned(),
        })
    }
}

/// Resolve a requested token against the cask catalog, honoring old-token renames.
pub(crate) fn resolve<'a>(ctx: &'a Ctx, requested: &str) -> Result<&'a Cask, OpError> {
    ctx.casks.get(requested).ok_or_else(|| OpError::Refusal {
        message: format!("Cask '{requested}' is unavailable."),
    })
}

fn acquire_locks(ctx: &Ctx, tokens: &BTreeSet<String>) -> Result<Vec<LockGuard>, OpError> {
    fs::create_dir_all(&ctx.env.locks)
        .map_err(|source| OpError::io("create", &ctx.env.locks, source))?;
    tokens
        .iter()
        .map(|token| {
            LockGuard::acquire(&ctx.env.locks, &format!("{token}.cask.lock")).map_err(Into::into)
        })
        .collect()
}

fn unique_stage(ctx: &Ctx, token: &str) -> Result<Utf8PathBuf, OpError> {
    let root = ctx.env.caskroom.join(".staging");
    fs::create_dir_all(&root).map_err(|source| OpError::io("create", &root, source))?;
    loop {
        let id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("{token}-{}-{id}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(OpError::io("create", path, source)),
        }
    }
}

fn string_values(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::String(value) => Some(vec![value.clone()]),
        Value::Array(values) => values
            .iter()
            .map(|value| value.as_str().map(str::to_owned))
            .collect(),
        _ => None,
    }
}

fn first_source(artifact: &zapbrew_api::CaskArtifact) -> Option<String> {
    let value = artifact.value.get(&artifact.kind)?;
    match value {
        Value::String(source) => Some(source.clone()),
        Value::Array(values) => values.first()?.as_str().map(str::to_owned),
        _ => None,
    }
}

fn artifact_target(artifact: &zapbrew_api::CaskArtifact) -> Option<String> {
    if let Some(target) = artifact.value.get("target").and_then(Value::as_str) {
        return Some(target.to_owned());
    }
    artifact
        .value
        .get(&artifact.kind)
        .and_then(Value::as_array)
        .and_then(|values| values.get(1))
        .and_then(Value::as_object)
        .and_then(|object| object.get("target"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn expand_path(ctx: &Ctx, raw: &str, appdir: &Utf8Path) -> Result<Utf8PathBuf, OpError> {
    let expanded = if raw == "~" {
        ctx.env.home.to_string()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        ctx.env.home.join(rest).to_string()
    } else {
        raw.replace("$HOME", ctx.env.home.as_str())
            .replace("${HOME}", ctx.env.home.as_str())
            .replace("$HOMEBREW_PREFIX", ctx.env.prefix.as_str())
            .replace("${HOMEBREW_PREFIX}", ctx.env.prefix.as_str())
            .replace("$APPDIR", appdir.as_str())
            .replace("${APPDIR}", appdir.as_str())
    };
    let path = Utf8PathBuf::from(expanded);
    if !path.is_absolute() || !safe_lexical(path.as_std_path()) {
        return Err(OpError::Refusal {
            message: format!("Cask path '{raw}' is unsafe."),
        });
    }
    Ok(path)
}

/// The fixed default application directory, mirroring Homebrew's `--appdir`.
pub(super) const DEFAULT_APPDIR: &str = "/Applications";

/// Validate an install-time `--appdir` value. The appdir is a record-trust
/// root: uninstall and force-replace treat it as a removal/confinement root,
/// so a broad value like `/` would make a tampered record able to delete
/// arbitrary user-writable paths. Only the fixed `/Applications` tree plus
/// roots under the user's home or prefix are accepted.
pub(super) fn approved_appdir(env: &zapbrew_prefix::Env, appdir: &Utf8Path) -> bool {
    if !appdir.is_absolute() || !safe_lexical(appdir.as_std_path()) {
        return false;
    }
    appdir.starts_with(Utf8Path::new(DEFAULT_APPDIR))
        || appdir.starts_with(&env.home)
        || appdir.starts_with(&env.prefix)
}

fn safe_relative(raw: &str) -> bool {
    !raw.is_empty() && !Path::new(raw).is_absolute() && safe_lexical(Path::new(raw))
}

fn safe_lexical(path: &Path) -> bool {
    path.components().all(|component| {
        !matches!(
            component,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir
        ) || path.is_absolute() && matches!(component, Component::RootDir)
    }) && !path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn path_exists(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// Resolve a raw (non-catalog) token to its exact Caskroom child, confining it to
/// one nonempty normal relative path component. Empties, `.`, `..`, separators,
/// absolute paths, and nested paths are rejected before any join so a traversal
/// token can never leave the Caskroom.
fn confined_caskroom_child(ctx: &Ctx, token: &str) -> Result<Utf8PathBuf, OpError> {
    let mut components = Utf8Path::new(token).components();
    let single = matches!(
        (components.next(), components.next()),
        (Some(Utf8Component::Normal(name)), None) if name == token
    );
    if !single {
        return Err(OpError::Refusal {
            message: format!("Cask '{token}' is unavailable."),
        });
    }
    Ok(ctx.env.caskroom.join(token))
}

fn remove_entry(path: &Utf8Path) -> Result<(), OpError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(OpError::io("inspect", path, source)),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path).map_err(|source| OpError::io("remove", path, source))
    } else {
        fs::remove_file(path).map_err(|source| OpError::io("remove", path, source))
    }
}

fn copy_entry(source: &Utf8Path, target: &Utf8Path) -> Result<(), OpError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|error| OpError::io("inspect", source, error))?;
    if metadata.file_type().is_symlink() {
        return Err(OpError::Refusal {
            message: format!("Cask source '{source}' is a symlink."),
        });
    }
    if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|error| OpError::io("create", target, error))?;
        let mut entries = fs::read_dir(source)
            .map_err(|error| OpError::io("read", source, error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| OpError::io("read", source, error))?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| OpError::InvalidState {
                reason: format!("non-UTF-8 cask source in {source}"),
            })?;
            copy_entry(&source.join(name), &target.join(name))?;
        }
    } else {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|error| OpError::io("create", parent, error))?;
        }
        fs::copy(source, target).map_err(|error| OpError::io("copy", target, error))?;
    }
    Ok(())
}

fn checked_command(ctx: &Ctx, spec: CommandSpec) -> Result<Vec<u8>, OpError> {
    let program = spec.program().to_string_lossy().into_owned();
    let output = ctx.commands.run(&spec).map_err(|source| OpError::Io {
        operation: "run",
        path: Utf8PathBuf::from(program.clone()),
        source,
    })?;
    if !output.success() {
        return Err(OpError::CommandFailed {
            program,
            status: output.status().to_string(),
            stderr: String::from_utf8_lossy(output.stderr()).trim().to_owned(),
        });
    }
    Ok(output.stdout().to_vec())
}

fn unsupported(token: &str, kind: &str) -> OpError {
    OpError::Refusal {
        message: format!("Cask '{token}' uses unsupported artifact '{kind}'."),
    }
}
