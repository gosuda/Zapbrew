//! Prefix layout helpers: racks, kegs, pins, linked/opt records, and locks.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Component, Path};
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_types::{FormulaName, PkgVersion};

use crate::env::Env;
use crate::error::PrefixError;

const RECEIPT_FILENAME: &str = "INSTALL_RECEIPT.json";
const DEFAULT_LOCK_COMMAND: &str = "brew";

/// Homebrew prefix view derived from a detected [`Env`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix {
    env: Env,
}

impl Prefix {
    /// Wrap a detected environment.
    #[must_use]
    pub fn new(env: Env) -> Self {
        Self { env }
    }

    /// Borrow the underlying environment.
    #[must_use]
    pub fn env(&self) -> &Env {
        &self.env
    }

    /// `HOMEBREW_PREFIX`.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.env.prefix
    }

    /// Cellar directory.
    #[must_use]
    pub fn cellar(&self) -> &Utf8Path {
        &self.env.cellar
    }

    /// Lock directory (`var/homebrew/locks`).
    #[must_use]
    pub fn locks(&self) -> &Utf8Path {
        &self.env.locks
    }

    /// Pin directory (`var/homebrew/pinned`).
    #[must_use]
    pub fn pins(&self) -> &Utf8Path {
        &self.env.pins
    }

    /// Linked-keg registry (`var/homebrew/linked`).
    #[must_use]
    pub fn linked(&self) -> &Utf8Path {
        &self.env.linked
    }

    /// `prefix/opt`.
    #[must_use]
    pub fn opt(&self) -> Utf8PathBuf {
        self.env.prefix.join("opt")
    }

    /// `prefix/bin`.
    #[must_use]
    pub fn bin(&self) -> Utf8PathBuf {
        self.env.prefix.join("bin")
    }

    /// `prefix/sbin`.
    #[must_use]
    pub fn sbin(&self) -> Utf8PathBuf {
        self.env.prefix.join("sbin")
    }

    /// `prefix/etc`.
    #[must_use]
    pub fn etc(&self) -> Utf8PathBuf {
        self.env.prefix.join("etc")
    }

    /// `prefix/var`.
    #[must_use]
    pub fn var(&self) -> Utf8PathBuf {
        self.env.prefix.join("var")
    }

    /// `prefix/share`.
    #[must_use]
    pub fn share(&self) -> Utf8PathBuf {
        self.env.prefix.join("share")
    }

    /// `prefix/lib`.
    #[must_use]
    pub fn lib(&self) -> Utf8PathBuf {
        self.env.prefix.join("lib")
    }

    /// `prefix/include`.
    #[must_use]
    pub fn include(&self) -> Utf8PathBuf {
        self.env.prefix.join("include")
    }

    /// Path of the linked-keg record for `name`.
    pub fn linked_path(&self, name: &FormulaName) -> Result<Utf8PathBuf, PrefixError> {
        linked_path(self.linked(), name)
    }

    /// Path of the opt record for `name`.
    pub fn opt_path(&self, name: &FormulaName) -> Result<Utf8PathBuf, PrefixError> {
        opt_path(self.path(), name)
    }
}

/// A formula rack: `Cellar/<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rack {
    path: Utf8PathBuf,
}

impl Rack {
    /// Build a rack path under `cellar` for `name`.
    pub fn new(cellar: &Utf8Path, name: &FormulaName) -> Result<Self, PrefixError> {
        validate_segment("formula", name.name())?;
        Ok(Self {
            path: cellar.join(name.name()),
        })
    }

    /// Enumerate cellar racks that contain at least one keg directory, sorted
    /// by formula name.
    pub fn all(cellar: &Utf8Path) -> Result<Vec<Self>, PrefixError> {
        if !cellar.exists() {
            return Ok(Vec::new());
        }

        let mut racks = Vec::new();
        for entry in read_dir_utf8(cellar)? {
            if !entry.is_dir() {
                continue;
            }
            if has_subdirectory(&entry)? {
                racks.push(Self { path: entry });
            }
        }

        racks.sort_by(|left, right| left.name().cmp(right.name()));
        Ok(racks)
    }

    /// Formula name segment (`Cellar/<name>`).
    #[must_use]
    pub fn name(&self) -> &str {
        self.path.file_name().unwrap_or(self.path.as_str())
    }

    /// Absolute rack path.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// Enumerate kegs under this rack, sorted by version directory name.
    pub fn kegs(&self) -> Result<Vec<Keg>, PrefixError> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let formula = parse_formula_name(self.name(), &self.path)?;
        let mut kegs = Vec::new();
        for entry in read_dir_utf8(&self.path)? {
            if !entry.is_dir() {
                continue;
            }
            let version_name = entry
                .file_name()
                .ok_or_else(|| invalid_data("read", &entry, "keg directory missing file name"))?;
            let version = parse_pkg_version(version_name, &entry)?;
            kegs.push(Keg {
                name: formula.clone(),
                version,
                path: entry,
            });
        }

        kegs.sort_by(|left, right| {
            left.version
                .cmp(&right.version)
                .then_with(|| left.path.as_str().cmp(right.path.as_str()))
        });
        Ok(kegs)
    }
}

/// An installed keg: `Cellar/<name>/<version>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keg {
    name: FormulaName,
    version: PkgVersion,
    path: Utf8PathBuf,
}

impl Keg {
    /// Construct a keg under `cellar` for `name` / `version`.
    pub fn new(
        cellar: &Utf8Path,
        name: FormulaName,
        version: PkgVersion,
    ) -> Result<Self, PrefixError> {
        validate_segment("formula", name.name())?;
        let version_str = version.to_string();
        validate_segment("version", &version_str)?;
        let path = cellar.join(name.name()).join(&version_str);
        Ok(Self {
            name,
            version,
            path,
        })
    }

    /// Formula name.
    #[must_use]
    pub fn name(&self) -> &FormulaName {
        &self.name
    }

    /// Package version (including revision).
    #[must_use]
    pub fn version(&self) -> &PkgVersion {
        &self.version
    }

    /// Absolute keg path.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    /// `INSTALL_RECEIPT.json` path inside this keg.
    #[must_use]
    pub fn receipt_path(&self) -> Utf8PathBuf {
        self.path.join(RECEIPT_FILENAME)
    }
}

/// Advisory prefix lock held for the lifetime of the guard.
///
/// The underlying [`File`] is kept open so the advisory lock is released when
/// the guard (and thus the file descriptor) is dropped. Both exclusive
/// ([`acquire`][LockGuard::acquire]) and shared
/// ([`acquire_shared`][LockGuard::acquire_shared]) locks are supported.
#[derive(Debug)]
pub struct LockGuard {
    path: Utf8PathBuf,
    _file: File,
}

impl LockGuard {
    /// Create `locks_dir` / `lock_file_name` if needed and acquire a nonblocking
    /// exclusive lock.
    ///
    /// `lock_file_name` is typically `"<rack>.formula.lock"`. Contention maps to
    /// [`PrefixError::LockBusy`] with command `"brew"`.
    pub fn acquire(locks_dir: &Utf8Path, lock_file_name: &str) -> Result<Self, PrefixError> {
        Self::open(locks_dir, lock_file_name).and_then(|(path, file)| match file.try_lock() {
            Ok(()) => Ok(Self { path, _file: file }),
            Err(TryLockError::WouldBlock) => Err(PrefixError::LockBusy {
                command: DEFAULT_LOCK_COMMAND.to_owned(),
                path,
            }),
            Err(TryLockError::Error(source)) => Err(PrefixError::io("lock", &path, source)),
        })
    }

    /// Create `locks_dir` / `lock_file_name` if needed and acquire a nonblocking
    /// **shared** (reader) lock.
    ///
    /// Multiple shared locks may be held concurrently on the same lock file, but
    /// neither a shared nor an exclusive lock can be acquired while an exclusive
    /// lock is held, and vice versa. Contention maps to
    /// [`PrefixError::LockBusy`] with command `"brew"`.
    pub fn acquire_shared(locks_dir: &Utf8Path, lock_file_name: &str) -> Result<Self, PrefixError> {
        Self::open(locks_dir, lock_file_name).and_then(|(path, file)| {
            match file.try_lock_shared() {
                Ok(()) => Ok(Self { path, _file: file }),
                Err(TryLockError::WouldBlock) => Err(PrefixError::LockBusy {
                    command: DEFAULT_LOCK_COMMAND.to_owned(),
                    path,
                }),
                Err(TryLockError::Error(source)) => Err(PrefixError::io("lock", &path, source)),
            }
        })
    }

    /// Open (creating if needed) the lock file under `locks_dir`, returning the
    /// full path and open [`File`].
    fn open(
        locks_dir: &Utf8Path,
        lock_file_name: &str,
    ) -> Result<(Utf8PathBuf, File), PrefixError> {
        validate_segment("lock", lock_file_name)?;

        fs::create_dir_all(locks_dir)
            .map_err(|source| PrefixError::io("create", locks_dir, source))?;

        let path = locks_dir.join(lock_file_name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.as_std_path())
            .map_err(|source| PrefixError::io("open", &path, source))?;

        Ok((path, file))
    }

    /// Path of the lock file.
    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }
}

/// Create `pins_dir/<name>` as the exact relative symlink
/// `../../../Cellar/<name>/<version>`.
///
/// No-op when a pin symlink already exists.
pub fn pin(pins_dir: &Utf8Path, keg: &Keg) -> Result<(), PrefixError> {
    validate_segment("formula", keg.name().name())?;
    let version_str = keg.version().to_string();
    validate_segment("version", &version_str)?;

    if !keg.path().is_dir() {
        return Err(PrefixError::io(
            "pin",
            keg.path(),
            io::Error::new(io::ErrorKind::NotFound, "keg directory not found"),
        ));
    }

    fs::create_dir_all(pins_dir).map_err(|source| PrefixError::io("create", pins_dir, source))?;

    let link = pins_dir.join(keg.name().name());
    if link
        .symlink_metadata()
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Ok(());
    }

    let target = pin_relative_target(keg);
    symlink_path(Path::new(&target), link.as_std_path())
        .map_err(|source| PrefixError::io("symlink", &link, source))
}

/// Remove `pins_dir/<name>` when it is a symlink. Idempotent when absent.
pub fn unpin(pins_dir: &Utf8Path, name: &FormulaName) -> Result<(), PrefixError> {
    validate_segment("formula", name.name())?;
    let link = pins_dir.join(name.name());
    match link.symlink_metadata() {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::remove_file(link.as_std_path())
                .map_err(|source| PrefixError::io("remove", &link, source))?;
        }
        Ok(_) | Err(_) => {}
    }

    // Best-effort cleanup matching brew's `rmdir_if_possible`.
    let _ = fs::remove_dir(pins_dir.as_std_path());
    Ok(())
}

/// True when `pins_dir/<name>` is a symlink whose target equals
/// `../../../Cellar/<name>/<version>`.
pub fn is_pinned(
    pins_dir: &Utf8Path,
    name: &FormulaName,
    version: &PkgVersion,
) -> Result<bool, PrefixError> {
    validate_segment("formula", name.name())?;
    let version_str = version.to_string();
    validate_segment("version", &version_str)?;
    let expected = format!("../../../Cellar/{}/{version}", name.name());
    let link = pins_dir.join(name.name());
    match fs::read_link(link.as_std_path()) {
        Ok(target) => Ok(target == Path::new(&expected)),
        Err(_) => Ok(false),
    }
}

/// Exact relative pin target written by [`pin`].
pub fn pin_relative_target(keg: &Keg) -> String {
    format!("../../../Cellar/{}/{}", keg.name().name(), keg.version())
}

/// `linked_dir/<name>` record path.
pub fn linked_path(linked_dir: &Utf8Path, name: &FormulaName) -> Result<Utf8PathBuf, PrefixError> {
    validate_segment("formula", name.name())?;
    Ok(linked_dir.join(name.name()))
}

/// `prefix/opt/<name>` record path.
pub fn opt_path(prefix: &Utf8Path, name: &FormulaName) -> Result<Utf8PathBuf, PrefixError> {
    validate_segment("formula", name.name())?;
    Ok(prefix.join("opt").join(name.name()))
}

/// Resolve `var/homebrew/linked/<name>` when it is a symlink.
pub fn resolve_linked(
    linked_dir: &Utf8Path,
    name: &FormulaName,
) -> Result<Option<Utf8PathBuf>, PrefixError> {
    let link = linked_path(linked_dir, name)?;
    Ok(resolve_symlink_record(&link))
}

/// Resolve `prefix/opt/<name>` when it is a symlink.
pub fn resolve_opt(
    prefix: &Utf8Path,
    name: &FormulaName,
) -> Result<Option<Utf8PathBuf>, PrefixError> {
    let link = opt_path(prefix, name)?;
    Ok(resolve_symlink_record(&link))
}

/// Reject path-traversal in a single path segment: accept only when `value`
/// parses as exactly one [`Component::Normal`].
fn validate_segment(kind: &'static str, value: &str) -> Result<(), PrefixError> {
    let mut components = Path::new(value).components();
    if matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none() {
        Ok(())
    } else {
        Err(PrefixError::InvalidPathSegment {
            kind,
            value: value.to_owned(),
        })
    }
}

fn resolve_symlink_record(link: &Utf8Path) -> Option<Utf8PathBuf> {
    let target = fs::read_link(link.as_std_path()).ok()?;
    let target = Utf8PathBuf::from_path_buf(target).ok()?;
    if target.is_absolute() {
        Some(target)
    } else {
        Some(link.parent()?.join(target))
    }
}

fn has_subdirectory(dir: &Utf8Path) -> Result<bool, PrefixError> {
    for entry in read_dir_utf8(dir)? {
        if entry.is_dir() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn read_dir_utf8(dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, PrefixError> {
    let mut paths = Vec::new();
    let iter =
        fs::read_dir(dir.as_std_path()).map_err(|source| PrefixError::io("read", dir, source))?;
    for entry in iter {
        let entry = entry.map_err(|source| PrefixError::io("read", dir, source))?;
        let path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| {
            invalid_data("read", dir, format!("non-utf8 path {}", path.display()))
        })?;
        paths.push(path);
    }
    Ok(paths)
}

fn parse_formula_name(name: &str, path: &Utf8Path) -> Result<FormulaName, PrefixError> {
    FormulaName::from_str(name).map_err(|err| invalid_data("parse", path, err))
}

fn parse_pkg_version(version: &str, path: &Utf8Path) -> Result<PkgVersion, PrefixError> {
    PkgVersion::from_str(version).map_err(|err| invalid_data("parse", path, err))
}

fn invalid_data(operation: &'static str, path: &Utf8Path, err: impl ToString) -> PrefixError {
    PrefixError::io(
        operation,
        path,
        io::Error::new(io::ErrorKind::InvalidData, err.to_string()),
    )
}

#[cfg(unix)]
fn symlink_path(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_path(target: &Path, link: &Path) -> io::Result<()> {
    // Pin/linked/opt records point at keg directories.
    std::os::windows::fs::symlink_dir(target, link)
}
