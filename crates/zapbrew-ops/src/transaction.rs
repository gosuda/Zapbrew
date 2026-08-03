use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_api::Formula;
use zapbrew_net::CachedBottle;
use zapbrew_pour::{LinkOptions, link, relocate, unlink, unpack};
use zapbrew_prefix::{Env, Keg, LockGuard, Prefix, Tab};
use zapbrew_types::{BottleFile, FormulaName};

use crate::{Ctx, OpError};

static TRANSACTION_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) struct Replacement {
    pub linked: Option<Keg>,
    pub target: Option<Keg>,
}

pub(crate) struct InstallInput<'a> {
    pub formula: &'a Formula,
    pub bottle: &'a BottleFile,
    pub cached: &'a CachedBottle,
    pub tab: Tab,
    pub replacement: Replacement,
}

pub(crate) struct Summary {
    pub keg: Utf8PathBuf,
    pub files: usize,
    pub size: u64,
}

#[derive(Default)]
struct Journal {
    created_dirs: Vec<Utf8PathBuf>,
    stage_root: Option<Utf8PathBuf>,
    promoted: bool,
    new_link_attempted: bool,
    skeleton: Vec<SkeletonEntry>,
    backup: Option<(Utf8PathBuf, Utf8PathBuf)>,
    old_unlinked: Option<Keg>,
}

struct SkeletonEntry {
    path: Utf8PathBuf,
    directory: bool,
}

pub(crate) fn acquire_formula_locks(
    env: &Env,
    names: &BTreeSet<String>,
) -> Result<Vec<LockGuard>, OpError> {
    ensure_directory(&env.locks, &env.prefix, None)?;
    let mut guards = Vec::with_capacity(names.len());
    for name in names {
        guards.push(LockGuard::acquire(
            &env.locks,
            &format!("{name}.formula.lock"),
        )?);
    }
    Ok(guards)
}

pub(crate) fn install(ctx: &Ctx, input: InstallInput<'_>) -> Result<Summary, OpError> {
    let name = input
        .formula
        .name
        .parse::<FormulaName>()
        .map_err(|source| OpError::InvalidState {
            reason: format!(
                "catalog formula name {} is invalid: {source}",
                input.formula.name
            ),
        })?;
    let final_keg = Keg::new(
        &ctx.env.cellar,
        name.clone(),
        input.formula.pkg_version.clone(),
    )?;
    let mut transaction = FormulaTransaction {
        ctx,
        input,
        name,
        final_keg,
        journal: Journal::default(),
    };
    match transaction.apply() {
        Ok(summary) => Ok(summary),
        Err(original) => Err(transaction.rollback(original)),
    }
}

struct FormulaTransaction<'a> {
    ctx: &'a Ctx,
    input: InstallInput<'a>,
    name: FormulaName,
    final_keg: Keg,
    journal: Journal,
}

impl FormulaTransaction<'_> {
    fn apply(&mut self) -> Result<Summary, OpError> {
        let rack = self.ctx.env.cellar.join(self.name.name());
        ensure_directory(
            &rack,
            &self.ctx.env.cellar,
            Some(&mut self.journal.created_dirs),
        )?;

        let id = TRANSACTION_ID.fetch_add(1, Ordering::Relaxed);
        let stage_root = rack.join(format!(".zapbrew-stage-{}-{id}", std::process::id()));
        self.journal.stage_root = Some(stage_root.clone());
        ensure_directory(
            &stage_root,
            &self.ctx.env.cellar,
            Some(&mut self.journal.created_dirs),
        )?;

        let staged = unpack(
            &self.input.cached.path,
            &stage_root,
            &self.name,
            &self.input.formula.pkg_version,
        )?;
        let relocation = relocate(
            &staged,
            &self.ctx.env,
            &self.input.bottle.cellar,
            self.ctx.commands.as_ref(),
        )?;
        self.input.tab.changed_files = Some(
            relocation
                .changed_files
                .into_iter()
                .map(|path| path.as_str().to_owned())
                .collect(),
        );
        self.input.tab.write(staged.receipt_path())?;
        let (files, size) = inventory(staged.path())?;

        if let Some(linked) = self.input.replacement.linked.clone() {
            self.journal.old_unlinked = Some(linked.clone());
            unlink(&linked, &Prefix::new(self.ctx.env.clone()))?;
        }

        if let Some(target) = self.input.replacement.target.as_ref() {
            let backup = rack.join(format!(
                ".zapbrew-backup-{}-{}-{id}",
                target.version(),
                std::process::id()
            ));
            self.journal.backup = Some((target.path().to_path_buf(), backup.clone()));
            safe_rename(&self.ctx.env, target.path(), &backup)?;
        }

        self.journal.promoted = true;
        safe_rename(&self.ctx.env, staged.path(), self.final_keg.path())?;
        remove_stage_shell(&self.ctx.env, &stage_root)?;
        self.journal.stage_root = None;

        self.journal.new_link_attempted = true;
        let report = link(
            &self.final_keg,
            &Prefix::new(self.ctx.env.clone()),
            LinkOptions {
                keg_only: self.input.formula.keg_only,
                ..LinkOptions::default()
            },
        )?;
        if !report.conflicts.is_empty() {
            return Err(OpError::Refusal {
                message: format!(
                    "Could not link {} because these paths already exist:\n  {}",
                    self.input.formula.name,
                    report
                        .conflicts
                        .iter()
                        .map(|path| path.as_str())
                        .collect::<Vec<_>>()
                        .join("\n  ")
                ),
            });
        }

        copy_skeleton(
            &self.ctx.env,
            self.final_keg.path(),
            &mut self.journal.skeleton,
        )?;

        if let Some((_, backup)) = self.journal.backup.as_ref() {
            safe_remove_tree(&self.ctx.env, backup)?;
            self.journal.backup = None;
        }
        self.journal.old_unlinked = None;
        self.journal.created_dirs.clear();

        Ok(Summary {
            keg: self.final_keg.path().to_path_buf(),
            files,
            size,
        })
    }

    fn rollback(&mut self, original: OpError) -> OpError {
        let mut leftovers = Vec::new();

        for entry in self.journal.skeleton.iter().rev() {
            if safe_remove_skeleton(&self.ctx.env, entry).is_err() && path_entry_exists(&entry.path)
            {
                leftovers.push(entry.path.clone());
            }
        }

        if self.journal.new_link_attempted
            && unlink(&self.final_keg, &Prefix::new(self.ctx.env.clone())).is_err()
        {
            collect_symlinks_to(&self.ctx.env.prefix, self.final_keg.path(), &mut leftovers);
        }

        if self.journal.promoted
            && path_entry_exists(self.final_keg.path())
            && safe_remove_tree(&self.ctx.env, self.final_keg.path()).is_err()
        {
            leftovers.push(self.final_keg.path().to_path_buf());
        }

        if let Some((original_path, backup)) = self.journal.backup.as_ref()
            && path_entry_exists(backup)
            && safe_rename(&self.ctx.env, backup, original_path).is_err()
        {
            leftovers.push(backup.clone());
        }

        if let Some(old) = self.journal.old_unlinked.as_ref()
            && link(
                old,
                &Prefix::new(self.ctx.env.clone()),
                LinkOptions::default(),
            )
            .is_err()
        {
            leftovers.push(old.path().to_path_buf());
            if let Ok(path) = Prefix::new(self.ctx.env.clone()).linked_path(old.name()) {
                leftovers.push(path);
            }
            if let Ok(path) = Prefix::new(self.ctx.env.clone()).opt_path(old.name()) {
                leftovers.push(path);
            }
        }

        if let Some(stage_root) = self.journal.stage_root.as_ref()
            && path_entry_exists(stage_root)
            && safe_remove_tree(&self.ctx.env, stage_root).is_err()
        {
            leftovers.push(stage_root.clone());
        }

        for directory in self.journal.created_dirs.iter().rev() {
            if safe_remove_empty_dir(&self.ctx.env, directory).is_err()
                && path_entry_exists(directory)
            {
                leftovers.push(directory.clone());
            }
        }

        leftovers.sort();
        leftovers.dedup();
        if leftovers.is_empty() {
            original
        } else {
            OpError::RollbackIncomplete {
                original: Box::new(original),
                leftovers: leftovers
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            }
        }
    }
}

fn copy_skeleton(
    env: &Env,
    keg: &Utf8Path,
    journal: &mut Vec<SkeletonEntry>,
) -> Result<(), OpError> {
    let bottle = keg.join(".bottle");
    for top in ["etc", "var"] {
        let source = bottle.join(top);
        if !source.exists() {
            continue;
        }
        let destination = env.prefix.join(top);
        copy_absent_tree(env, &source, &destination, journal)?;
    }
    Ok(())
}

fn copy_absent_tree(
    env: &Env,
    source: &Utf8Path,
    destination: &Utf8Path,
    journal: &mut Vec<SkeletonEntry>,
) -> Result<(), OpError> {
    let metadata = fs::symlink_metadata(source.as_std_path())
        .map_err(|source_error| OpError::io("inspect", source.to_path_buf(), source_error))?;
    if metadata.file_type().is_symlink() {
        return Err(OpError::InvalidState {
            reason: format!("bottle skeleton contains symlink {source}"),
        });
    }
    if metadata.is_dir() {
        if path_entry_exists(destination) {
            let destination_metadata =
                fs::symlink_metadata(destination.as_std_path()).map_err(|source_error| {
                    OpError::io("inspect", destination.to_path_buf(), source_error)
                })?;
            if destination_metadata.file_type().is_symlink() || !destination_metadata.is_dir() {
                return Err(OpError::InvalidState {
                    reason: format!("skeleton destination is not a directory: {destination}"),
                });
            }
        } else {
            ensure_confined(destination, &[&env.prefix])?;
            journal.push(SkeletonEntry {
                path: destination.to_path_buf(),
                directory: true,
            });
            fs::create_dir(destination.as_std_path()).map_err(|source_error| {
                OpError::io("create", destination.to_path_buf(), source_error)
            })?;
        }
        let mut entries = read_dir(source)?;
        entries.sort();
        for entry in entries {
            let file_name = entry.file_name().ok_or_else(|| OpError::InvalidState {
                reason: format!("skeleton entry has no file name: {entry}"),
            })?;
            copy_absent_tree(env, &entry, &destination.join(file_name), journal)?;
        }
    } else if metadata.is_file() && !path_entry_exists(destination) {
        ensure_confined(destination, &[&env.prefix])?;
        journal.push(SkeletonEntry {
            path: destination.to_path_buf(),
            directory: false,
        });
        fs::copy(source.as_std_path(), destination.as_std_path())
            .map_err(|source_error| OpError::io("copy", destination.to_path_buf(), source_error))?;
    }
    Ok(())
}

fn inventory(root: &Utf8Path) -> Result<(usize, u64), OpError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = 0usize;
    let mut size = 0u64;
    while let Some(directory) = pending.pop() {
        for path in read_dir(&directory)? {
            let metadata = fs::symlink_metadata(path.as_std_path())
                .map_err(|source| OpError::io("inspect", path.clone(), source))?;
            if metadata.is_dir() {
                pending.push(path);
            } else {
                files = files.saturating_add(1);
                if metadata.is_file() {
                    size = size.saturating_add(metadata.len());
                }
            }
        }
    }
    Ok((files, size))
}

fn read_dir(path: &Utf8Path) -> Result<Vec<Utf8PathBuf>, OpError> {
    let entries = fs::read_dir(path.as_std_path())
        .map_err(|source| OpError::io("read", path.to_path_buf(), source))?;
    entries
        .map(|entry| {
            let entry = entry.map_err(|source| OpError::io("read", path.to_path_buf(), source))?;
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("path is not UTF-8: {}", path.display()),
            })
        })
        .collect()
}

fn remove_stage_shell(env: &Env, stage_root: &Utf8Path) -> Result<(), OpError> {
    safe_remove_tree(env, stage_root)
}

fn safe_rename(env: &Env, source: &Utf8Path, destination: &Utf8Path) -> Result<(), OpError> {
    ensure_confined(source, &[&env.cellar])?;
    ensure_confined(destination, &[&env.cellar])?;
    let metadata = fs::symlink_metadata(source.as_std_path())
        .map_err(|error| OpError::io("inspect", source.to_path_buf(), error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(OpError::InvalidState {
            reason: format!("rename source is not a real directory: {source}"),
        });
    }
    if path_entry_exists(destination) {
        return Err(OpError::InvalidState {
            reason: format!("rename destination already exists: {destination}"),
        });
    }
    fs::rename(source.as_std_path(), destination.as_std_path())
        .map_err(|error| OpError::io("rename", destination.to_path_buf(), error))
}

fn safe_remove_tree(env: &Env, path: &Utf8Path) -> Result<(), OpError> {
    ensure_confined(path, &[&env.cellar])?;
    let metadata = fs::symlink_metadata(path.as_std_path())
        .map_err(|error| OpError::io("inspect", path.to_path_buf(), error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(OpError::InvalidState {
            reason: format!("remove target is not a real directory: {path}"),
        });
    }
    fs::remove_dir_all(path.as_std_path())
        .map_err(|error| OpError::io("remove", path.to_path_buf(), error))
}

fn safe_remove_skeleton(env: &Env, entry: &SkeletonEntry) -> Result<(), OpError> {
    ensure_confined(&entry.path, &[&env.prefix])?;
    let metadata = match fs::symlink_metadata(entry.path.as_std_path()) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(OpError::io("inspect", entry.path.clone(), error)),
    };
    if metadata.file_type().is_symlink()
        || (entry.directory && !metadata.is_dir())
        || (!entry.directory && !metadata.is_file())
    {
        return Err(OpError::InvalidState {
            reason: format!("rollback target changed type: {}", entry.path),
        });
    }
    let result = if entry.directory {
        fs::remove_dir(entry.path.as_std_path())
    } else {
        fs::remove_file(entry.path.as_std_path())
    };
    result.map_err(|error| OpError::io("remove", entry.path.clone(), error))
}

fn safe_remove_empty_dir(env: &Env, path: &Utf8Path) -> Result<(), OpError> {
    ensure_confined(path, &[&env.cellar])?;
    let metadata = match fs::symlink_metadata(path.as_std_path()) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(OpError::io("inspect", path.to_path_buf(), error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(OpError::InvalidState {
            reason: format!("rollback directory changed type: {path}"),
        });
    }
    match fs::remove_dir(path.as_std_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(error) => Err(OpError::io("remove", path.to_path_buf(), error)),
    }
}

fn path_entry_exists(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path.as_std_path()).is_ok()
}

fn ensure_existing_directory(path: &Utf8Path) -> Result<(), OpError> {
    let mut current = PathBuf::new();
    for component in path.as_std_path().components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(OpError::InvalidState {
                    reason: format!("Env path contains parent traversal: {path}"),
                });
            }
        }
        let current_utf8 =
            Utf8PathBuf::from_path_buf(current.clone()).map_err(|path| OpError::InvalidState {
                reason: format!("Env ancestor is not UTF-8: {}", path.display()),
            })?;
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| OpError::io("inspect", current_utf8, error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(OpError::InvalidState {
                reason: format!(
                    "Env ancestor is not a real directory: {}",
                    current.display()
                ),
            });
        }
    }
    Ok(())
}

fn ensure_directory(
    path: &Utf8Path,
    root: &Utf8Path,
    mut journal: Option<&mut Vec<Utf8PathBuf>>,
) -> Result<(), OpError> {
    ensure_confined(path, &[root])?;
    let root_parent = root.parent().ok_or_else(|| OpError::InvalidState {
        reason: format!("Env root has no parent: {root}"),
    })?;
    ensure_existing_directory(root_parent)?;
    let relative = path
        .strip_prefix(root_parent)
        .map_err(|_| OpError::InvalidState {
            reason: format!("mutation path {path} is not below Env root parent {root_parent}"),
        })?;
    let mut current = root_parent.to_path_buf();
    for component in relative.components() {
        if !matches!(component, camino::Utf8Component::Normal(_)) {
            return Err(OpError::InvalidState {
                reason: format!("mutation path contains non-normal component: {path}"),
            });
        }
        current.push(component.as_str());
        match fs::symlink_metadata(current.as_std_path()) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(OpError::InvalidState {
                    reason: format!("directory ancestor is not a real directory: {current}"),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if let Some(entries) = journal.as_deref_mut() {
                    entries.push(current.clone());
                }
                fs::create_dir(current.as_std_path())
                    .map_err(|source| OpError::io("create", current.clone(), source))?;
            }
            Err(error) => return Err(OpError::io("inspect", current.clone(), error)),
        }
    }
    Ok(())
}

fn ensure_confined(path: &Utf8Path, roots: &[&Utf8Path]) -> Result<(), OpError> {
    if !path.is_absolute()
        || !roots
            .iter()
            .any(|root| path == *root || path.strip_prefix(root).is_ok())
    {
        return Err(OpError::InvalidState {
            reason: format!("mutation path escapes Env: {path}"),
        });
    }
    let parent = path.parent().ok_or_else(|| OpError::InvalidState {
        reason: format!("mutation path has no parent: {path}"),
    })?;
    let mut current = PathBuf::new();
    for component in parent.as_std_path().components() {
        match component {
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                current.push(component.as_os_str());
            }
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(OpError::InvalidState {
                    reason: format!("mutation path contains parent traversal: {path}"),
                });
            }
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(OpError::InvalidState {
                    reason: format!(
                        "mutation ancestor is not a real directory: {}",
                        current.display()
                    ),
                });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(OpError::io(
                    "inspect",
                    Utf8PathBuf::from_path_buf(current.clone()).map_err(|path| {
                        OpError::InvalidState {
                            reason: format!("path is not UTF-8: {}", path.display()),
                        }
                    })?,
                    error,
                ));
            }
        }
    }
    Ok(())
}

fn collect_symlinks_to(root: &Utf8Path, target: &Utf8Path, leftovers: &mut Vec<Utf8PathBuf>) {
    let Ok(entries) = read_dir(root) else {
        return;
    };
    for path in entries {
        let Ok(metadata) = fs::symlink_metadata(path.as_std_path()) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            if fs::canonicalize(path.as_std_path())
                .ok()
                .is_some_and(|resolved| resolved.starts_with(target.as_std_path()))
            {
                leftovers.push(path);
            }
        } else if metadata.is_dir() {
            collect_symlinks_to(&path, target, leftovers);
        }
    }
}
