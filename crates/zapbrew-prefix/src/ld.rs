//! Linux dynamic-linker bootstrap: `<prefix>/lib/ld.so` resolution, mirroring
//! brew's `symlink_ld_so` (extend/os/linux/install.rb) and `OS::Linux::Ld`
//! (os/linux/ld.rb). Every install refreshes this link so relocated binaries
//! with a `<prefix>/lib/ld.so` interpreter resolve on a fresh prefix.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use rustix::fs::{Access, access};
use zapbrew_types::BottleTag;

use crate::env::Env;
use crate::error::PrefixError;

/// Known paths to host dynamic linkers (brew `DYNAMIC_LINKERS`).
const DYNAMIC_LINKERS: &[&str] = &[
    "/lib64/ld-linux-x86-64.so.2",
    "/lib64/ld64.so.2",
    "/lib/ld-linux.so.3",
    "/lib/ld-linux.so.2",
    "/lib/ld-linux-aarch64.so.1",
    "/lib/ld-linux-armhf.so.3",
    "/system/bin/linker64",
    "/system/bin/linker",
];

/// Ensure `<prefix>/lib/ld.so` is a symlink to a usable dynamic linker.
///
/// Prefers a brewed glibc's linker (`<prefix>/opt/glibc/bin/ld.so`), else the
/// first executable host linker from [`DYNAMIC_LINKERS`]. No-op on non-Linux
/// hosts, when an existing readable link already points at the chosen target,
/// and (like brew) when no linker is discoverable but the existing link is
/// still readable. Refuses with [`PrefixError::NoSystemLdSo`] only when no
/// linker can be found and nothing usable exists yet.
pub fn symlink_ld_so(env: &Env) -> Result<(), PrefixError> {
    if !matches!(env.bottle_tag, BottleTag::Linux { .. }) {
        return Ok(());
    }
    let brew_ld_so = env.prefix.join("lib/ld.so");
    let brewed = env.prefix.join("opt/glibc/bin/ld.so");
    let target = if readable(&brewed) {
        brewed
    } else {
        match system_ld_so() {
            Some(linker) => linker,
            None => {
                if !readable(&brew_ld_so) {
                    return Err(PrefixError::NoSystemLdSo);
                }
                return Ok(());
            }
        }
    };

    if readable(&brew_ld_so) && read_link_target(&brew_ld_so).as_deref() == Some(target.as_str()) {
        return Ok(());
    }

    let parent = brew_ld_so.parent().ok_or_else(|| {
        PrefixError::io(
            "create parent",
            &brew_ld_so,
            std::io::Error::other("path has no parent"),
        )
    })?;
    fs::create_dir_all(parent.as_std_path())
        .map_err(|source| PrefixError::io("create parent", parent, source))?;
    // `FileUtils.ln_sf` semantics: replace whatever occupies the path.
    match fs::symlink_metadata(brew_ld_so.as_std_path()) {
        Ok(_) => {
            fs::remove_file(brew_ld_so.as_std_path())
                .map_err(|source| PrefixError::io("remove", &brew_ld_so, source))?;
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(PrefixError::io("inspect", &brew_ld_so, source)),
    }
    symlink(target.as_std_path(), brew_ld_so.as_std_path())
        .map_err(|source| PrefixError::io("symlink", &brew_ld_so, source))
}

/// First executable host linker from [`DYNAMIC_LINKERS`], if any.
fn system_ld_so() -> Option<Utf8PathBuf> {
    DYNAMIC_LINKERS
        .iter()
        .find(|candidate| {
            Utf8Path::new(candidate).is_absolute()
                && access(Path::new(candidate), Access::EXEC_OK).is_ok()
        })
        .map(|candidate| Utf8PathBuf::from(candidate.to_owned()))
}

/// True when `path` resolves to a readable file.
fn readable(path: &Utf8Path) -> bool {
    access(path.as_std_path(), Access::READ_OK).is_ok()
}

/// The raw link target of a symlink, as a string.
fn read_link_target(link: &Utf8Path) -> Option<String> {
    let target = fs::read_link(link.as_std_path()).ok()?;
    target.to_str().map(str::to_owned)
}
