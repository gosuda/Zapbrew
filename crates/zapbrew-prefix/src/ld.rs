//! Linux dynamic-linker bootstrap: `<prefix>/lib/ld.so` resolution and the
//! preferred-GCC `ld.so.conf` snippet, mirroring brew's `symlink_ld_so` /
//! `setup_preferred_gcc_libs` (extend/os/linux/install.rb) and `OS::Linux::Ld`
//! (os/linux/ld.rb). Every install refreshes the link so relocated binaries
//! with a `<prefix>/lib/ld.so` interpreter resolve on a fresh prefix, and
//! writes the conf snippet so the brewed glibc's `ldconfig` indexes the
//! preferred GCC runtime libraries.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::Path;

use camino::{Utf8Path, Utf8PathBuf};
use rustix::fs::{Access, access};
use zapbrew_types::BottleTag;

use crate::env::Env;
use crate::error::PrefixError;

/// Preferred GCC runtime formula name (brew `LINUX_PREFERRED_GCC_RUNTIME_FORMULA`).
const PREFERRED_GCC_RUNTIME_FORMULA: &str = "gcc";

/// Conf snippet filename under `<prefix>/etc/ld.so.conf.d/` (brew
/// `50-homebrew-preferred-gcc.conf`).
const LD_SO_CONF_D_FILENAME: &str = "50-homebrew-preferred-gcc.conf";

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

/// Write the preferred-GCC `ld.so.conf` snippet for relocated Linux bottles.
///
/// Mirrors brew's `setup_preferred_gcc_libs` (extend/os/linux/install.rb):
/// when a brewed glibc (`<prefix>/opt/glibc/bin/ld.so`) and a brewed preferred
/// GCC (`<prefix>/opt/gcc`) are both readable, ensure
/// `<prefix>/etc/ld.so.conf.d/50-homebrew-preferred-gcc.conf` lists
/// `<prefix>/opt/gcc/lib/gcc/current` so the brewed glibc's `ldconfig` indexes
/// the GCC runtime libraries (`libstdc++.so.6`, `libgcc_s.so.1`, …) at runtime.
///
/// No-op on non-Linux hosts, when no brewed GCC is installed, or when no
/// brewed glibc is present (without a prefix-local `ldconfig` there is no conf
/// to feed; the runpath rewriter in `zapbrew-pour` covers that case). Idempotent:
/// the file is rewritten — and the stale `ld.so.cache` rebuilt — only when its
/// content would change.
pub fn setup_preferred_gcc_libs(env: &Env) -> Result<(), PrefixError> {
    if !matches!(env.bottle_tag, BottleTag::Linux { .. }) {
        return Ok(());
    }
    let gcc_opt_prefix = env.prefix.join("opt").join(PREFERRED_GCC_RUNTIME_FORMULA);
    // brew: `return unless gcc_opt_prefix.readable?`
    if !readable(&gcc_opt_prefix) {
        return Ok(());
    }
    // Without a brewed glibc there is no prefix-local ldconfig to feed, so
    // brew instead symlinks the runtime libs into `<prefix>/lib` — already
    // handled by the runpath rewriter in `zapbrew-pour`. Nothing to write.
    let glibc_ld_so = env.prefix.join("opt/glibc/bin/ld.so");
    if !readable(&glibc_ld_so) {
        return Ok(());
    }

    let ld_so_conf_d = env.prefix.join("etc/ld.so.conf.d");
    if !ld_so_conf_d.exists() {
        fs::create_dir_all(ld_so_conf_d.as_std_path())
            .map_err(|source| PrefixError::io("create", &ld_so_conf_d, source))?;
        // brew: `FileUtils.chmod "go-w", ld_so_conf_d` -> rwxr-xr-x.
        set_mode(&ld_so_conf_d, 0o755)?;
    }

    let conf = ld_so_conf_d.join(LD_SO_CONF_D_FILENAME);
    let gcc_lib_dir = gcc_opt_prefix.join("lib/gcc/current");
    let content = format!("# This file is generated by Homebrew. Do not modify.\n{gcc_lib_dir}\n");

    let needs_write = match fs::read_to_string(conf.as_std_path()) {
        Ok(existing) => existing != content,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => true,
        Err(source) => return Err(PrefixError::io("read", &conf, source)),
    };
    if needs_write {
        atomic_write(&conf, content.as_bytes(), 0o644)?;
        // brew: drop the stale cache and let the brewed glibc rebuild it.
        let cache = env.prefix.join("etc/ld.so.cache");
        let _ = fs::remove_file(cache.as_std_path());
        let ldconfig = env.prefix.join("opt/glibc/sbin/ldconfig");
        if readable(&ldconfig) {
            // Best-effort: brew ignores `Kernel.system` failures here.
            let _ = std::process::Command::new(ldconfig.as_std_path()).status();
        }
    }
    Ok(())
}

/// Set the filesystem mode of `path` to `mode`.
fn set_mode(path: &Utf8Path, mode: u32) -> Result<(), PrefixError> {
    fs::set_permissions(path.as_std_path(), fs::Permissions::from_mode(mode))
        .map_err(|source| PrefixError::io("chmod", path, source))
}

/// Atomically write `bytes` to `path` with `mode` (temp file + rename), so a
/// concurrent reader never observes a partial conf file (brew `atomic_write`).
fn atomic_write(path: &Utf8Path, bytes: &[u8], mode: u32) -> Result<(), PrefixError> {
    let parent = path.parent().ok_or_else(|| {
        PrefixError::io("write", path, std::io::Error::other("path has no parent"))
    })?;
    let file_name = path.file_name().unwrap_or("conf");
    let tmp = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));
    {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(tmp.as_std_path())
            .map_err(|source| PrefixError::io("create", &tmp, source))?;
        file.write_all(bytes)
            .map_err(|source| PrefixError::io("write", &tmp, source))?;
        file.sync_all()
            .map_err(|source| PrefixError::io("sync", &tmp, source))?;
    }
    fs::rename(tmp.as_std_path(), path.as_std_path())
        .map_err(|source| PrefixError::io("rename", path, source))
}
