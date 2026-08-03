//! Secure bottle archive unpack into the Cellar.
//!
//! Bottles are gzip-compressed tar streams (`1f 8b`). Every entry is validated
//! before any filesystem mutation; only then is the archive reopened and
//! selected regular files, directories, and contained links unpacked under
//! `Cellar/<name>/<version>/`.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};
use flate2::read::GzDecoder;
use tar::{Archive, EntryType};
use zapbrew_prefix::Keg;
use zapbrew_types::{FormulaName, PkgVersion};

use crate::error::PourError;

/// Unpack a gzip bottle tarball into `cellar`, returning the installed [`Keg`].
///
/// Preflight validates every tar entry (path prefix, traversal, link targets,
/// and forbidden entry types) without writing. Only after a clean preflight
/// does this reopen the archive and unpack regular files, directories, and
/// contained hard/symbolic links. Extracted directories receive owner-write
/// (`u+w`) while other mode bits are preserved.
pub fn unpack(
    tarball: &Utf8Path,
    cellar: &Utf8Path,
    name: &FormulaName,
    version: &PkgVersion,
) -> Result<Keg, PourError> {
    let keg = Keg::new(cellar, name.clone(), version.clone())?;
    let name_str = name.name();
    let version_str = version.to_string();

    assert_gzip_magic(tarball)?;
    preflight(tarball, cellar, keg.path(), name_str, &version_str)?;

    // `tar::Entry::unpack_in` canonicalizes the destination root, so the cellar
    // must exist before any entry is written. Created only after a clean preflight.
    fs::create_dir_all(cellar.as_std_path())
        .map_err(|source| PourError::io("create", cellar, source))?;

    extract(tarball, cellar, keg.path(), name_str, &version_str)?;
    ensure_owner_writable_dirs(keg.path())?;

    Ok(keg)
}

fn assert_gzip_magic(tarball: &Utf8Path) -> Result<(), PourError> {
    let mut file = File::open(tarball.as_std_path())
        .map_err(|source| PourError::io("open", tarball, source))?;
    let mut magic = [0_u8; 2];
    file.read_exact(&mut magic)
        .map_err(|source| PourError::io("read", tarball, source))?;
    if magic != [0x1f, 0x8b] {
        return Err(invalid_archive(
            tarball.as_str(),
            "not a gzip-compressed bottle (expected 1f 8b magic)",
        ));
    }
    Ok(())
}

fn open_archive(tarball: &Utf8Path) -> Result<Archive<GzDecoder<File>>, PourError> {
    let file = File::open(tarball.as_std_path())
        .map_err(|source| PourError::io("open", tarball, source))?;
    Ok(Archive::new(GzDecoder::new(file)))
}

fn preflight(
    tarball: &Utf8Path,
    cellar: &Utf8Path,
    keg_path: &Utf8Path,
    name: &str,
    version: &str,
) -> Result<(), PourError> {
    let mut archive = open_archive(tarball)?;
    let entries = archive
        .entries()
        .map_err(|source| PourError::io("read", tarball, source))?;

    for entry in entries {
        let entry = entry.map_err(|source| PourError::io("read", tarball, source))?;
        let path = entry_path_utf8(&entry)?;
        classify_entry(entry.header().entry_type(), path.as_str())?;
        match entry.header().entry_type() {
            EntryType::Regular
            | EntryType::Continuous
            | EntryType::Directory
            | EntryType::Symlink
            | EntryType::Link => {
                validate_entry_path(path.as_std_path(), name, version)?;
                validate_destination(cellar, keg_path, path.as_std_path())?;
                if matches!(
                    entry.header().entry_type(),
                    EntryType::Symlink | EntryType::Link
                ) {
                    validate_link(&entry, path.as_std_path(), cellar, keg_path, name, version)?;
                }
            }
            EntryType::XHeader
            | EntryType::XGlobalHeader
            | EntryType::GNULongName
            | EntryType::GNULongLink => {
                // Consumed by the tar crate for metadata; nothing to validate.
            }
            other => {
                return Err(invalid_archive(
                    path.as_str(),
                    format!("unsafe entry type {other:?} (devices and FIFOs are forbidden)"),
                ));
            }
        }
    }

    Ok(())
}

fn extract(
    tarball: &Utf8Path,
    cellar: &Utf8Path,
    keg_path: &Utf8Path,
    name: &str,
    version: &str,
) -> Result<(), PourError> {
    let mut archive = open_archive(tarball)?;
    let entries = archive
        .entries()
        .map_err(|source| PourError::io("read", tarball, source))?;

    for entry in entries {
        let mut entry = entry.map_err(|source| PourError::io("read", tarball, source))?;
        let path = entry_path_utf8(&entry)?;
        let entry_type = entry.header().entry_type();

        match entry_type {
            EntryType::Regular
            | EntryType::Continuous
            | EntryType::Directory
            | EntryType::Symlink
            | EntryType::Link => {
                // Defense in depth: re-check before mutation.
                validate_entry_path(path.as_std_path(), name, version)?;
                validate_destination(cellar, keg_path, path.as_std_path())?;
                if matches!(entry_type, EntryType::Symlink | EntryType::Link) {
                    validate_link(&entry, path.as_std_path(), cellar, keg_path, name, version)?;
                }

                let unpacked = entry
                    .unpack_in(cellar.as_std_path())
                    .map_err(|source| PourError::io("unpack", cellar.join(&path), source))?;
                if !unpacked {
                    return Err(invalid_archive(
                        path.as_str(),
                        "tar entry skipped during unpack (path rejected by unpack_in)",
                    ));
                }

                // Apply u+w as directories appear so a later entry is not blocked by an
                // archive mode that omitted owner-write on a parent directory.
                if matches!(entry_type, EntryType::Directory) {
                    set_owner_writable(&cellar.join(&path))?;
                }
            }
            EntryType::XHeader
            | EntryType::XGlobalHeader
            | EntryType::GNULongName
            | EntryType::GNULongLink => {}
            other => {
                return Err(invalid_archive(
                    path.as_str(),
                    format!("unsafe entry type {other:?} (devices and FIFOs are forbidden)"),
                ));
            }
        }
    }

    Ok(())
}

fn classify_entry(entry_type: EntryType, path: &str) -> Result<(), PourError> {
    match entry_type {
        EntryType::Regular
        | EntryType::Continuous
        | EntryType::Directory
        | EntryType::Symlink
        | EntryType::Link
        | EntryType::XHeader
        | EntryType::XGlobalHeader
        | EntryType::GNULongName
        | EntryType::GNULongLink => Ok(()),
        EntryType::Char | EntryType::Block | EntryType::Fifo => Err(invalid_archive(
            path,
            format!("unsafe entry type {entry_type:?} (devices and FIFOs are forbidden)"),
        )),
        other => Err(invalid_archive(
            path,
            format!("unsafe entry type {other:?} (devices and FIFOs are forbidden)"),
        )),
    }
}

fn entry_path_utf8<R: Read>(entry: &tar::Entry<'_, R>) -> Result<Utf8PathBuf, PourError> {
    let path = entry
        .path()
        .map_err(|source| PourError::io("read", Utf8PathBuf::from("<entry>"), source))?;
    Utf8PathBuf::from_path_buf(path.into_owned()).map_err(|path| {
        invalid_archive(path.display().to_string(), "entry path is not valid UTF-8")
    })
}

fn validate_entry_path(path: &Path, name: &str, version: &str) -> Result<(), PourError> {
    let display = path.display().to_string();

    // `Path::components()` drops mid-path `CurDir`, so inspect raw `/`-segments for `.`
    // and non-trailing empty parts (`//`) before the component walk.
    let raw_parts: Vec<&str> = display.split('/').collect();
    for (index, part) in raw_parts.iter().enumerate() {
        if *part == "." {
            return Err(invalid_archive(
                display,
                "current directory component (`.`) is forbidden",
            ));
        }
        if part.is_empty() {
            let trailing_dir_slash = index + 1 == raw_parts.len();
            let leading_absolute = index == 0;
            if !trailing_dir_slash && !leading_absolute {
                return Err(invalid_archive(display, "empty path segment is forbidden"));
            }
        }
    }

    let mut normals: Vec<&std::ffi::OsStr> = Vec::new();

    for component in path.components() {
        match component {
            Component::Normal(part) => normals.push(part),
            Component::ParentDir => {
                return Err(invalid_archive(
                    display,
                    "parent directory component (`..`) is forbidden",
                ));
            }
            Component::CurDir => {
                return Err(invalid_archive(
                    display,
                    "current directory component (`.`) is forbidden",
                ));
            }
            Component::RootDir => {
                return Err(invalid_archive(display, "absolute path is forbidden"));
            }
            Component::Prefix(_) => {
                return Err(invalid_archive(
                    display,
                    "Windows prefix component is forbidden",
                ));
            }
        }
    }

    if normals.len() < 2 {
        return Err(invalid_archive(
            display,
            format!("expected first components `{name}/{version}`"),
        ));
    }

    let first = normals[0].to_str().ok_or_else(|| {
        invalid_archive(display.clone(), "entry path component is not valid UTF-8")
    })?;
    let second = normals[1].to_str().ok_or_else(|| {
        invalid_archive(display.clone(), "entry path component is not valid UTF-8")
    })?;

    if first != name || second != version {
        return Err(invalid_archive(
            display,
            format!("expected first components `{name}/{version}`, found `{first}/{second}`"),
        ));
    }

    Ok(())
}

fn validate_destination(
    cellar: &Utf8Path,
    keg_path: &Utf8Path,
    entry_path: &Path,
) -> Result<(), PourError> {
    let dest = cellar.as_std_path().join(entry_path);
    let display = entry_path.display().to_string();

    if !dest.starts_with(cellar.as_std_path()) {
        return Err(invalid_archive(display, "destination escapes cellar root"));
    }
    if !dest.starts_with(keg_path.as_std_path()) {
        return Err(invalid_archive(display, "destination escapes keg prefix"));
    }
    Ok(())
}

fn validate_link<R: Read>(
    entry: &tar::Entry<'_, R>,
    entry_path: &Path,
    cellar: &Utf8Path,
    keg_path: &Utf8Path,
    name: &str,
    version: &str,
) -> Result<(), PourError> {
    let display = entry_path.display().to_string();
    let link = entry
        .link_name()
        .map_err(|source| PourError::io("read", Utf8PathBuf::from(&display), source))?
        .ok_or_else(|| invalid_archive(display.clone(), "link entry missing link target"))?;

    if link.as_os_str().is_empty() {
        return Err(invalid_archive(display, "link target is empty"));
    }

    match entry.header().entry_type() {
        EntryType::Link => {
            // Hard-link targets are archive-root-relative paths.
            validate_entry_path(link.as_ref(), name, version)?;
            validate_destination(cellar, keg_path, link.as_ref())?;
        }
        EntryType::Symlink => {
            validate_symlink_target(entry_path, link.as_ref(), cellar, keg_path)?;
        }
        _ => {}
    }

    Ok(())
}

fn validate_symlink_target(
    entry_path: &Path,
    link_target: &Path,
    cellar: &Utf8Path,
    keg_path: &Utf8Path,
) -> Result<(), PourError> {
    let display = entry_path.display().to_string();

    let final_dest = if link_target.is_absolute() {
        link_target.to_path_buf()
    } else {
        let parent = entry_path.parent().unwrap_or_else(|| Path::new(""));
        let resolved_rel = lexical_join(parent, link_target).ok_or_else(|| {
            invalid_archive(display.clone(), "symbolic link target escapes keg via `..`")
        })?;
        cellar.as_std_path().join(resolved_rel)
    };

    if !final_dest.starts_with(keg_path.as_std_path()) {
        return Err(invalid_archive(
            display,
            "symbolic link target escapes keg prefix",
        ));
    }

    Ok(())
}

/// Lexically join `base`/`rel`, rejecting paths that climb above `base`'s root.
fn lexical_join(base: &Path, rel: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in base.components().chain(rel.components()) {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

fn ensure_owner_writable_dirs(root: &Utf8Path) -> Result<(), PourError> {
    if !root.exists() {
        return Ok(());
    }
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        set_owner_writable(&dir)?;
        let read = fs::read_dir(dir.as_std_path())
            .map_err(|source| PourError::io("read", &dir, source))?;
        for entry in read {
            let entry = entry.map_err(|source| PourError::io("read", &dir, source))?;
            let file_type = entry
                .file_type()
                .map_err(|source| PourError::io("metadata", &dir, source))?;
            if file_type.is_dir() {
                let path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| {
                    PourError::io(
                        "read",
                        &dir,
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("non-utf8 path {}", path.display()),
                        ),
                    )
                })?;
                stack.push(path);
            }
        }
    }
    Ok(())
}

fn set_owner_writable(path: &Utf8Path) -> Result<(), PourError> {
    let stat = rustix::fs::stat(path.as_std_path())
        .map_err(|source| PourError::io("metadata", path, io::Error::from(source)))?;
    // Preserve existing permission bits; only ensure owner-write (u+w / S_IWUSR).
    let mode = rustix::fs::Mode::from_raw_mode(stat.st_mode) | rustix::fs::Mode::WUSR;
    rustix::fs::chmod(path.as_std_path(), mode)
        .map_err(|source| PourError::io("chmod", path, io::Error::from(source)))?;
    Ok(())
}

fn invalid_archive(path: impl Into<String>, reason: impl Into<String>) -> PourError {
    PourError::InvalidArchive {
        path: path.into(),
        reason: reason.into(),
    }
}
