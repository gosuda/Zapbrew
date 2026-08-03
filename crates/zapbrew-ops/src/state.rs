use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

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

/// Scan every rack, keg, and receipt under `env` exactly once into an immutable snapshot.
pub fn scan(env: &Env) -> Result<InstalledState, OpError> {
    let mut formulae = BTreeMap::new();

    for rack in Rack::all(&env.cellar)? {
        let kegs = rack.kegs()?;
        let Some(first) = kegs.first() else {
            continue;
        };
        let name = first.name().clone();
        let linked_target = resolved_target(resolve_linked(&env.linked, &name)?);
        let opt_target = resolved_target(resolve_opt(&env.prefix, &name)?);
        let mut installed = Vec::with_capacity(kegs.len());

        for keg in kegs {
            let canonical_keg = fs::canonicalize(keg.path().as_std_path()).ok();
            let tab = Tab::load(keg.receipt_path())?;
            let (files, size) = inventory(keg.path())?;
            installed.push(InstalledKeg {
                version: keg.version().clone(),
                path: keg.path().to_path_buf(),
                tab,
                linked: same_target(linked_target.as_ref(), canonical_keg.as_ref()),
                opt: same_target(opt_target.as_ref(), canonical_keg.as_ref()),
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

fn resolved_target(target: Option<Utf8PathBuf>) -> Option<PathBuf> {
    target.and_then(|path| fs::canonicalize(path.as_std_path()).ok())
}

fn same_target(left: Option<&PathBuf>, right: Option<&PathBuf>) -> bool {
    matches!((left, right), (Some(left), Some(right)) if left == right)
}

fn dependency_matches(recorded: &str, requested: &str) -> bool {
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
