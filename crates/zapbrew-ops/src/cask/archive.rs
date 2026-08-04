use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};

use bzip2::read::BzDecoder;
use camino::{Utf8Path, Utf8PathBuf};
use flate2::read::GzDecoder;
use lzma_rust2::XzReader;
use plist::Value;
use ruzstd::decoding::StreamingDecoder;
use tar::{Archive, EntryType};
use zapbrew_prefix::CommandSpec;
use zip::ZipArchive;

use super::{checked_command, remove_entry, safe_relative};
use crate::{Ctx, OpError};

pub(super) fn extract(
    ctx: &Ctx,
    artifact: &Utf8Path,
    url: &str,
    staging: &Utf8Path,
) -> Result<(), OpError> {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();
    if path.ends_with(".dmg") {
        return extract_dmg(ctx, artifact, staging);
    }
    if path.ends_with(".zip") {
        extract_zip(artifact, staging)?;
    } else if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
        extract_tar(|| open_gzip(artifact), staging)?;
    } else if path.ends_with(".tar.bz2") || path.ends_with(".tbz2") {
        extract_tar(|| open_bzip2(artifact), staging)?;
    } else if path.ends_with(".tar.xz") || path.ends_with(".txz") {
        extract_tar(|| open_xz(artifact), staging)?;
    } else if path.ends_with(".tar.zst") || path.ends_with(".zst") {
        extract_tar(|| open_zstd(artifact), staging)?;
    } else {
        let name = artifact.file_name().ok_or_else(|| OpError::InvalidState {
            reason: format!("artifact has no basename: {artifact}"),
        })?;
        fs::copy(artifact, staging.join(name))
            .map_err(|source| OpError::io("copy", staging.join(name), source))?;
    }
    Ok(())
}

pub(super) fn detach(ctx: &Ctx, mount: &Utf8Path) -> Result<(), OpError> {
    checked_command(
        ctx,
        CommandSpec::new("/usr/bin/hdiutil")
            .arg("detach")
            .arg(mount.as_str()),
    )?;
    Ok(())
}

fn extract_dmg(ctx: &Ctx, artifact: &Utf8Path, staging: &Utf8Path) -> Result<(), OpError> {
    let mount_root = staging
        .parent()
        .ok_or_else(|| OpError::InvalidState {
            reason: format!("staging directory has no parent: {staging}"),
        })?
        .join(format!(".mount-{}", std::process::id()));
    fs::create_dir_all(&mount_root).map_err(|source| OpError::io("create", &mount_root, source))?;
    let stdout = checked_command(
        ctx,
        CommandSpec::new("/usr/bin/hdiutil")
            .arg("attach")
            .arg("-plist")
            .arg("-nobrowse")
            .arg("-readonly")
            .arg("-mountrandom")
            .arg(mount_root.as_str())
            .arg(artifact.as_str()),
    )?;
    let plist =
        Value::from_reader_xml(stdout.as_slice()).map_err(|error| OpError::InvalidState {
            reason: format!("invalid hdiutil attach plist: {error}"),
        })?;
    let mount = plist
        .as_dictionary()
        .and_then(|root| root.get("system-entities"))
        .and_then(Value::as_array)
        .and_then(|entities| {
            entities.iter().find_map(|entity| {
                entity
                    .as_dictionary()
                    .and_then(|item| item.get("mount-point"))
                    .and_then(Value::as_string)
            })
        })
        .map(Utf8PathBuf::from)
        .ok_or_else(|| OpError::InvalidState {
            reason: "hdiutil attach plist has no mount-point".to_owned(),
        })?;

    let result = checked_command(
        ctx,
        CommandSpec::new("/usr/bin/ditto")
            .arg(mount.as_str())
            .arg(staging.as_str()),
    );
    let detached = detach(ctx, &mount);
    let _ = remove_entry(&mount_root);
    match (result, detached) {
        (Ok(_), Ok(())) => Ok(()),
        (Err(original), _) => Err(original),
        (Ok(_), Err(error)) => Err(error),
    }
}

fn extract_zip(artifact: &Utf8Path, staging: &Utf8Path) -> Result<(), OpError> {
    preflight_zip(artifact)?;
    let file = File::open(artifact).map_err(|source| OpError::io("open", artifact, source))?;
    let mut archive = ZipArchive::new(file).map_err(|source| invalid_archive(artifact, source))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|source| invalid_archive(artifact, source))?;
        let enclosed = entry.enclosed_name().ok_or_else(|| OpError::InvalidState {
            reason: format!("unsafe zip entry in {artifact}"),
        })?;
        let relative = utf8_relative(&enclosed, artifact)?;
        let target = staging.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|source| OpError::io("create", &target, source))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|source| OpError::io("create", parent, source))?;
            }
            let mut output =
                File::create(&target).map_err(|source| OpError::io("create", &target, source))?;
            io::copy(&mut entry, &mut output)
                .map_err(|source| OpError::io("write", &target, source))?;
            if let Some(mode) = entry.unix_mode() {
                fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777))
                    .map_err(|source| OpError::io("chmod", &target, source))?;
            }
        }
    }
    Ok(())
}

fn preflight_zip(artifact: &Utf8Path) -> Result<(), OpError> {
    let file = File::open(artifact).map_err(|source| OpError::io("open", artifact, source))?;
    let mut archive = ZipArchive::new(file).map_err(|source| invalid_archive(artifact, source))?;
    let mut paths = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|source| invalid_archive(artifact, source))?;
        let enclosed = entry.enclosed_name().ok_or_else(|| OpError::InvalidState {
            reason: format!("unsafe zip entry in {artifact}"),
        })?;
        let relative = utf8_relative(&enclosed, artifact)?;
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(OpError::InvalidState {
                reason: format!("zip symlink entry is unsupported: {relative}"),
            });
        }
        reject_nested_file(&paths, Path::new(relative), artifact)?;
        if !entry.is_dir() {
            paths.insert(relative.to_owned());
        }
    }
    Ok(())
}

fn extract_tar<F>(mut open: F, staging: &Utf8Path) -> Result<(), OpError>
where
    F: FnMut() -> Result<Box<dyn Read>, OpError>,
{
    preflight_tar(open()?)?;
    let mut archive = Archive::new(open()?);
    let entries = archive
        .entries()
        .map_err(|source| OpError::io("read", staging, source))?;
    for entry in entries {
        let mut entry = entry.map_err(|source| OpError::io("read", staging, source))?;
        if metadata_type(entry.header().entry_type()) {
            continue;
        }
        let raw = entry
            .path()
            .map_err(|source| OpError::io("read", staging, source))?;
        let relative = utf8_relative(&raw, staging)?;
        let target = staging.join(relative);
        if entry.header().entry_type() == EntryType::Directory {
            fs::create_dir_all(&target).map_err(|source| OpError::io("create", &target, source))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .map_err(|source| OpError::io("create", parent, source))?;
            }
            entry
                .unpack(&target)
                .map_err(|source| OpError::io("unpack", &target, source))?;
        }
    }
    Ok(())
}

fn preflight_tar(reader: Box<dyn Read>) -> Result<(), OpError> {
    let mut archive = Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|source| OpError::io("read", "cask archive", source))?;
    let mut paths = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|source| OpError::io("read", "cask archive", source))?;
        let kind = entry.header().entry_type();
        if metadata_type(kind) {
            continue;
        }
        if !matches!(
            kind,
            EntryType::Regular | EntryType::Continuous | EntryType::Directory
        ) {
            return Err(OpError::InvalidState {
                reason: format!("unsafe cask archive entry type {kind:?}"),
            });
        }
        let raw = entry
            .path()
            .map_err(|source| OpError::io("read", "cask archive", source))?;
        let relative = utf8_relative(&raw, Utf8Path::new("cask archive"))?;
        reject_nested_file(&paths, Path::new(relative), Utf8Path::new("cask archive"))?;
        if kind != EntryType::Directory {
            paths.insert(relative.to_owned());
        }
    }
    Ok(())
}

fn metadata_type(kind: EntryType) -> bool {
    matches!(
        kind,
        EntryType::XHeader
            | EntryType::XGlobalHeader
            | EntryType::GNULongName
            | EntryType::GNULongLink
    )
}

fn reject_nested_file(
    paths: &BTreeSet<String>,
    path: &Path,
    archive: &Utf8Path,
) -> Result<(), OpError> {
    let mut parent = path.parent();
    while let Some(current) = parent {
        let text = current.to_string_lossy();
        if paths.contains(text.as_ref()) {
            return Err(OpError::InvalidState {
                reason: format!("archive entry nests below earlier file in {archive}"),
            });
        }
        parent = current.parent();
    }
    Ok(())
}

fn utf8_relative<'a>(path: &'a Path, archive: &Utf8Path) -> Result<&'a str, OpError> {
    let raw = path.to_str().ok_or_else(|| OpError::InvalidState {
        reason: format!("non-UTF-8 archive entry in {archive}"),
    })?;
    if !safe_relative(raw)
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(OpError::InvalidState {
            reason: format!("unsafe archive entry '{raw}' in {archive}"),
        });
    }
    Ok(raw)
}

fn open_gzip(path: &Utf8Path) -> Result<Box<dyn Read>, OpError> {
    Ok(Box::new(GzDecoder::new(open_file(path)?)))
}

fn open_bzip2(path: &Utf8Path) -> Result<Box<dyn Read>, OpError> {
    Ok(Box::new(BzDecoder::new(open_file(path)?)))
}

fn open_xz(path: &Utf8Path) -> Result<Box<dyn Read>, OpError> {
    Ok(Box::new(XzReader::new(open_file(path)?, true)))
}

fn open_zstd(path: &Utf8Path) -> Result<Box<dyn Read>, OpError> {
    let decoder =
        StreamingDecoder::new(open_file(path)?).map_err(|source| OpError::InvalidState {
            reason: format!("invalid zstd cask archive {path}: {source}"),
        })?;
    Ok(Box::new(decoder))
}

fn open_file(path: &Utf8Path) -> Result<File, OpError> {
    File::open(path).map_err(|source| OpError::io("open", path, source))
}

fn invalid_archive(path: &Utf8Path, source: impl std::fmt::Display) -> OpError {
    OpError::InvalidState {
        reason: format!("invalid cask archive {path}: {source}"),
    }
}
