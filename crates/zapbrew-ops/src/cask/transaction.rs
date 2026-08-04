use std::fs;
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use jiff::Timestamp;
use zapbrew_api::Cask;
use zapbrew_types::Checksum;

use super::archive;
use super::artifact::{self, Plan, Reverse};
use super::{path_exists, remove_entry, unique_stage};
use crate::{Ctx, OpError};

/// One fully preflighted cask install request.
pub(super) struct CaskInstall<'a> {
    pub cask: &'a Cask,
    pub plan: &'a Plan,
    pub version: &'a str,
    pub url: &'a str,
    pub checksum: Option<&'a Checksum>,
    pub appdir: &'a Utf8Path,
    pub force: bool,
}

/// Execute one fully preflighted install as a journaled transaction.
pub(super) async fn install(ctx: &Ctx, request: CaskInstall<'_>) -> Result<(), OpError> {
    let CaskInstall {
        cask,
        plan,
        version,
        url,
        checksum,
        appdir,
        force,
    } = request;
    let final_dir = ctx.env.caskroom.join(&cask.token).join(version);
    if path_exists(&final_dir) && !force {
        ctx.reporter
            .opoo(&format!("Cask '{}' is already installed.", cask.token));
        return Ok(());
    }

    let cached = zapbrew_net::fetch_artifact(&ctx.env, &ctx.http, url, checksum).await?;
    let staging = unique_stage(ctx, &cask.token)?;
    let mut journal = Vec::<Reverse>::new();
    let mut receipt = None;
    let mut old_version = None;
    let mut old_targets = None;

    let result = (|| {
        archive::extract(ctx, &cached.path, url, &staging)?;

        if path_exists(&final_dir) {
            // Force reinstall of an existing version: back up the prior deployed
            // artifacts (from the stored receipt) and version before replacing.
            let old_plan = super::uninstall::stored_plan(ctx, &cask.token, version, appdir)?;
            let backup_root = unique_stage(ctx, &format!("{}-replaced", cask.token))?;
            old_targets = Some(backup_root.clone());
            artifact::backup_old_targets(&old_plan, &backup_root, &mut journal)?;

            let backup = ctx.env.caskroom.join(".staging").join(format!(
                "{}-old-{}",
                cask.token,
                std::process::id()
            ));
            fs::rename(&final_dir, &backup)
                .map_err(|source| OpError::io("backup", &final_dir, source))?;
            old_version = Some(backup);
        }

        artifact::apply(ctx, plan, &staging, &final_dir, &mut journal)?;
        let written = write_receipt(ctx, cask, version)?;
        receipt = Some(written);
        if let Some(parent) = final_dir.parent() {
            fs::create_dir_all(parent).map_err(|source| OpError::io("create", parent, source))?;
        }
        fs::rename(&staging, &final_dir)
            .map_err(|source| OpError::io("promote", &final_dir, source))?;
        Ok(())
    })();

    match result {
        Ok(()) => {
            if let Some(old) = old_version {
                remove_entry(&old)?;
            }
            if let Some(backups) = old_targets {
                remove_entry(&backups)?;
            }
            Ok(())
        }
        Err(original) => {
            if path_exists(&final_dir) && !path_exists(&staging) {
                let _ = fs::rename(&final_dir, &staging);
            }
            let mut leftovers = artifact::rollback(&journal);
            if let Some(path) = receipt.as_ref()
                && remove_entry(path).is_err()
                && path_exists(path)
            {
                leftovers.push(path.to_string());
            }
            if remove_entry(&staging).is_err() && path_exists(&staging) {
                leftovers.push(staging.to_string());
            }
            if let Some(backups) = old_targets.as_ref() {
                // Best-effort: succeeds only once every restore drained the dir.
                let _ = fs::remove_dir(backups);
            }
            if let Some(old) = old_version
                && path_exists(&old)
                && fs::rename(&old, &final_dir).is_err()
            {
                leftovers.push(old.to_string());
            }
            if leftovers.is_empty() {
                Err(original)
            } else {
                leftovers.sort();
                leftovers.dedup();
                Err(OpError::RollbackIncomplete {
                    original: Box::new(original),
                    leftovers: leftovers.join(", "),
                })
            }
        }
    }
}

fn write_receipt(ctx: &Ctx, cask: &Cask, version: &str) -> Result<Utf8PathBuf, OpError> {
    let timestamp = Timestamp::now().strftime("%Y%m%d%H%M%S").to_string();
    let path = ctx
        .env
        .caskroom
        .join(&cask.token)
        .join(".metadata")
        .join(version)
        .join(timestamp)
        .join("Casks")
        .join(format!("{}.json", cask.token));
    let parent = path.parent().ok_or_else(|| OpError::InvalidState {
        reason: format!("receipt path has no parent: {path}"),
    })?;
    fs::create_dir_all(parent).map_err(|source| OpError::io("create", parent, source))?;
    let mut bytes =
        serde_json::to_vec_pretty(&cask.raw).map_err(|source| OpError::InvalidState {
            reason: format!("could not serialize cask receipt: {source}"),
        })?;
    bytes.push(b'\n');
    fs::write(&path, bytes).map_err(|source| OpError::io("write", &path, source))?;
    Ok(path)
}

pub(super) fn validate_download(
    cask: &Cask,
) -> Result<(String, String, Option<Checksum>), OpError> {
    let version = cask
        .version
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OpError::Refusal {
            message: format!("Cask '{}' has no version.", cask.token),
        })?;
    let url = cask
        .url
        .clone()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| OpError::Refusal {
            message: format!("Cask '{}' has no URL.", cask.token),
        })?;
    let declared = cask.sha256.as_deref().ok_or_else(|| OpError::Refusal {
        message: format!("Cask '{}' has no checksum.", cask.token),
    })?;
    let checksum = if declared == "no_check" {
        None
    } else {
        Some(Checksum::from_str(declared).map_err(|_| OpError::Refusal {
            message: format!("Cask '{}' has an invalid checksum.", cask.token),
        })?)
    };
    Ok((version, url, checksum))
}

pub(super) fn installed_version_dirs(token_dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, OpError> {
    let entries = match fs::read_dir(token_dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(OpError::io("read", token_dir, source)),
    };
    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| OpError::io("read", token_dir, source))?;
        let path =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("non-UTF-8 Caskroom path: {}", path.display()),
            })?;
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| OpError::io("inspect", &path, source))?;
        let name = path.file_name().unwrap_or_default();
        if metadata.is_dir() && !metadata.file_type().is_symlink() && !name.starts_with('.') {
            versions.push(path);
        }
    }
    versions.sort();
    Ok(versions)
}
