use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_prefix::{Env, Rack, Tab, is_pinned, resolve_linked, resolve_opt};
use zapbrew_types::{FormulaName, PkgVersion};

use crate::OpError;

/// One installed keg and the receipt-derived state observed during a scan.
#[derive(Debug, Clone)]
pub struct InstalledKeg {
    version: PkgVersion,
    path: Utf8PathBuf,
    tab: Tab,
    linked: bool,
    opt: bool,
    pinned: bool,
    files: Vec<Utf8PathBuf>,
    size: u64,
}

impl InstalledKeg {
    #[must_use]
    pub fn version(&self) -> &PkgVersion {
        &self.version
    }

    #[must_use]
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    #[must_use]
    pub fn tab(&self) -> &Tab {
        &self.tab
    }

    #[must_use]
    pub fn is_linked(&self) -> bool {
        self.linked
    }

    #[must_use]
    pub fn is_optlinked(&self) -> bool {
        self.opt
    }

    #[must_use]
    pub fn is_pinned(&self) -> bool {
        self.pinned
    }

    /// Files relative to the keg root, sorted by path.
    #[must_use]
    pub fn files(&self) -> &[Utf8PathBuf] {
        &self.files
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Sum of regular-file lengths in bytes. Symlinks count as files but add no bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Every installed keg for one formula, in semantic package-version order.
#[derive(Debug, Clone)]
pub struct InstalledFormula {
    name: FormulaName,
    kegs: Vec<InstalledKeg>,
}

impl InstalledFormula {
    #[must_use]
    pub fn name(&self) -> &FormulaName {
        &self.name
    }

    #[must_use]
    pub fn kegs(&self) -> &[InstalledKeg] {
        &self.kegs
    }

    #[must_use]
    pub fn latest(&self) -> Option<&InstalledKeg> {
        self.kegs.last()
    }

    #[must_use]
    pub fn linked(&self) -> Option<&InstalledKeg> {
        self.kegs.iter().find(|keg| keg.linked)
    }

    #[must_use]
    pub fn optlinked(&self) -> Option<&InstalledKeg> {
        self.kegs.iter().find(|keg| keg.opt)
    }

    #[must_use]
    pub fn pinned(&self) -> Option<&InstalledKeg> {
        self.kegs.iter().find(|keg| keg.pinned)
    }
}

/// Immutable snapshot of the installed formula state under one prefix.
#[derive(Debug, Clone, Default)]
pub struct InstalledState {
    formulae: BTreeMap<String, InstalledFormula>,
}

impl InstalledState {
    #[must_use]
    pub fn len(&self) -> usize {
        self.formulae.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.formulae.is_empty()
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.formulae.contains_key(name)
    }

    #[must_use]
    pub fn formula(&self, name: &str) -> Option<&InstalledFormula> {
        self.formulae.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &InstalledFormula> {
        self.formulae.values()
    }

    /// Installed formulae whose receipts name `name` as a runtime dependency.
    #[must_use]
    pub fn dependents_of(&self, name: &str) -> Vec<&InstalledFormula> {
        self.formulae
            .values()
            .filter(|formula| {
                formula.kegs.iter().any(|keg| {
                    keg.tab
                        .runtime_dependencies
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .any(|dependency| dependency_matches(&dependency.full_name, name))
                })
            })
            .collect()
    }
}

/// Installed versions observed for one Caskroom token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstalledCask {
    token: String,
    versions: Vec<String>,
}

impl InstalledCask {
    #[must_use]
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    #[must_use]
    pub(crate) fn installed_version(&self) -> Option<&str> {
        self.versions.last().map(String::as_str)
    }
}

/// Immutable, token-sorted snapshot of installed Caskroom state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InstalledCaskState {
    casks: BTreeMap<String, InstalledCask>,
}

impl InstalledCaskState {
    #[must_use]
    pub(crate) fn cask(&self, token: &str) -> Option<&InstalledCask> {
        self.casks.get(token)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &InstalledCask> {
        self.casks.values()
    }
}

/// Scan the Caskroom without following token or version symlinks.
pub(crate) fn scan_casks(env: &Env) -> Result<InstalledCaskState, OpError> {
    let entries = match fs::read_dir(&env.caskroom) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InstalledCaskState::default());
        }
        Err(source) => return Err(OpError::io("read", &env.caskroom, source)),
    };
    let mut casks = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|source| OpError::io("read", &env.caskroom, source))?;
        let path =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("non-UTF-8 Caskroom path: {}", path.display()),
            })?;
        let Some(token) = path.file_name() else {
            continue;
        };
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| OpError::io("inspect", &path, source))?;
        if token.starts_with('.') || !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let versions = cask_versions(&path)?;
        if !versions.is_empty() {
            casks.insert(
                token.to_owned(),
                InstalledCask {
                    token: token.to_owned(),
                    versions,
                },
            );
        }
    }
    Ok(InstalledCaskState { casks })
}

fn cask_versions(token_dir: &Utf8Path) -> Result<Vec<String>, OpError> {
    let entries =
        fs::read_dir(token_dir).map_err(|source| OpError::io("read", token_dir, source))?;
    let mut versions = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| OpError::io("read", token_dir, source))?;
        let path =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("non-UTF-8 Caskroom path: {}", path.display()),
            })?;
        let Some(version) = path.file_name() else {
            continue;
        };
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| OpError::io("inspect", &path, source))?;
        if !version.starts_with('.') && metadata.is_dir() && !metadata.file_type().is_symlink() {
            versions.push(version.to_owned());
        }
    }
    versions.sort();
    Ok(versions)
}

/// Scan every rack, keg, and receipt under `env` exactly once into an immutable snapshot.
pub fn scan(env: &Env) -> Result<InstalledState, OpError> {
    if !env.cellar.exists() {
        return Ok(InstalledState::default());
    }
    let canonical_cellar = fs::canonicalize(env.cellar.as_std_path())
        .map_err(|source| OpError::io("canonicalize", env.cellar.clone(), source))?;
    scan_racks(env, &canonical_cellar, Rack::all(&env.cellar)?)
}

/// Scan only formula racks protected by the caller's lock set.
pub(crate) fn scan_selected(
    env: &Env,
    names: &BTreeSet<String>,
) -> Result<InstalledState, OpError> {
    if !env.cellar.exists() {
        return Ok(InstalledState::default());
    }
    let canonical_cellar = fs::canonicalize(env.cellar.as_std_path())
        .map_err(|source| OpError::io("canonicalize", env.cellar.clone(), source))?;
    let mut racks = Vec::with_capacity(names.len());
    for name in names {
        let formula_name = FormulaName::from_str(name).map_err(|source| OpError::InvalidState {
            reason: format!("catalog formula name {name} is invalid: {source}"),
        })?;
        racks.push(Rack::new(&env.cellar, &formula_name)?);
    }
    scan_racks(env, &canonical_cellar, racks)
}

fn scan_racks(
    env: &Env,
    canonical_cellar: &Path,
    racks: Vec<Rack>,
) -> Result<InstalledState, OpError> {
    let mut formulae = BTreeMap::new();
    for rack in racks {
        match fs::symlink_metadata(rack.path().as_std_path()) {
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(OpError::io("inspect", rack.path().to_path_buf(), source)),
        }
        inspect_dir(rack.path(), "rack")?;

        let kegs = rack.kegs()?;
        let Some(first) = kegs.first() else {
            continue;
        };
        let name = first.name().clone();
        let linked_target = resolved_target(resolve_linked(&env.linked, &name)?);
        let opt_target = resolved_target(resolve_opt(&env.prefix, &name)?);
        let mut installed = Vec::with_capacity(kegs.len());

        for keg in kegs {
            inspect_dir(keg.path(), "keg")?;
            let canonical_keg = canonicalize_keg(keg.path(), canonical_cellar)?;
            let tab = Tab::load(keg.receipt_path())?;
            let (files, size) = inventory(keg.path())?;
            installed.push(InstalledKeg {
                version: keg.version().clone(),
                path: keg.path().to_path_buf(),
                tab,
                linked: same_target(linked_target.as_ref(), Some(&canonical_keg)),
                opt: same_target(opt_target.as_ref(), Some(&canonical_keg)),
                pinned: is_pinned(&env.pins, &name, keg.version())?,
                files,
                size,
            });
        }

        formulae.insert(
            name.name().to_owned(),
            InstalledFormula {
                name,
                kegs: installed,
            },
        );
    }
    Ok(InstalledState { formulae })
}

fn inspect_dir(path: &Utf8Path, kind: &'static str) -> Result<(), OpError> {
    let metadata = fs::symlink_metadata(path.as_std_path())
        .map_err(|source| OpError::io("inspect", path.to_path_buf(), source))?;
    if metadata.is_symlink() {
        return Err(OpError::InvalidState {
            reason: format!("{kind} is a symlink: {path}"),
        });
    }
    if !metadata.is_dir() {
        return Err(OpError::InvalidState {
            reason: format!("{kind} is not a directory: {path}"),
        });
    }
    Ok(())
}

fn canonicalize_keg(path: &Utf8Path, canonical_cellar: &Path) -> Result<PathBuf, OpError> {
    let canonical = fs::canonicalize(path.as_std_path())
        .map_err(|source| OpError::io("canonicalize", path.to_path_buf(), source))?;
    if !is_descendant(&canonical, canonical_cellar) {
        return Err(OpError::InvalidState {
            reason: format!("keg {path} escapes cellar {}", canonical_cellar.display()),
        });
    }
    Ok(canonical)
}

fn is_descendant(path: &Path, ancestor: &Path) -> bool {
    path.strip_prefix(ancestor)
        .is_ok_and(|rest| !rest.as_os_str().is_empty())
}

fn resolved_target(target: Option<Utf8PathBuf>) -> Option<PathBuf> {
    target.and_then(|path| fs::canonicalize(path.as_std_path()).ok())
}

fn same_target(left: Option<&PathBuf>, right: Option<&PathBuf>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left == right)
}

pub(crate) fn dependency_matches(recorded: &str, requested: &str) -> bool {
    recorded == requested || recorded.rsplit('/').next() == requested.rsplit('/').next()
}

fn inventory(root: &Utf8Path) -> Result<(Vec<Utf8PathBuf>, u64), OpError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut size = 0_u64;

    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(directory.as_std_path())
            .map_err(|source| OpError::io("read", directory.clone(), source))?;
        for entry in entries {
            let entry = entry.map_err(|source| OpError::io("read", directory.clone(), source))?;
            let path =
                Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                    reason: format!("installed path is not UTF-8: {}", path.display()),
                })?;
            let metadata = fs::symlink_metadata(path.as_std_path())
                .map_err(|source| OpError::io("inspect", path.clone(), source))?;
            if metadata.is_dir() {
                pending.push(path);
                continue;
            }

            let relative = path
                .strip_prefix(root)
                .map_err(|_| OpError::InvalidState {
                    reason: format!("installed file {path} escaped keg {root}"),
                })?
                .to_path_buf();
            files.push(relative);
            if metadata.is_file() {
                size = size.saturating_add(metadata.len());
            }
        }
    }

    files.sort();
    Ok((files, size))
}
