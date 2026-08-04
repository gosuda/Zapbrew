use camino::{Utf8Path, Utf8PathBuf};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Component, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use zapbrew_api::Formula;
use zapbrew_net::CachedBottle;
use zapbrew_pour::{LinkOptions, link, relocate, unlink, unpack};
use zapbrew_prefix::{Env, Keg, LockGuard, Prefix, Tab};
use zapbrew_types::{BottleFile, FormulaName};

use crate::install_steps::{InstallSteps, StepJournal, remove_tree_confined};
use crate::{Ctx, OpError};

static TRANSACTION_ID: AtomicU64 = AtomicU64::new(1);
static TEST_HOOKS: Mutex<TestHooks> = Mutex::new(TestHooks {
    stage: None,
    cleanup_failure_formula: None,
    install_failure_after_unlink: None,
    removal_failure_after: None,
});

struct TestHooks {
    stage: Option<(Utf8PathBuf, u64)>,
    cleanup_failure_formula: Option<String>,
    install_failure_after_unlink: Option<String>,
    removal_failure_after: Option<(String, usize)>,
}

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
    pub steps: &'a InstallSteps,
}

pub(crate) struct Summary {
    pub keg: Utf8PathBuf,
    pub files: usize,
    pub size: u64,
}

pub(crate) struct RemovalKeg {
    pub keg: Keg,
    pub linked: bool,
    pub optlinked: bool,
}

pub(crate) struct RemovalInput {
    pub name: FormulaName,
    pub targets: Vec<RemovalKeg>,
    pub remove_rack: bool,
}

struct SymlinkSnapshot {
    path: Utf8PathBuf,
    target: Option<PathBuf>,
}

struct RemovalRecord {
    rack: Utf8PathBuf,
    rack_removed: bool,
    opt: SymlinkSnapshot,
    linked: SymlinkSnapshot,
    pin: SymlinkSnapshot,
}

struct StagedRemoval {
    original: Utf8PathBuf,
    trash: Utf8PathBuf,
}

struct UnlinkedKeg {
    keg: Keg,
    keg_only: bool,
}

#[derive(Default)]
struct Journal {
    created_dirs: Vec<Utf8PathBuf>,
    stage_root: Option<Utf8PathBuf>,
    promoted: bool,
    new_link_attempted: bool,
    skeleton: Vec<SkeletonEntry>,
    backup: Option<(Utf8PathBuf, Utf8PathBuf)>,
    old_unlinked: Option<UnlinkedKeg>,
    steps: StepJournal,
    committed: bool,
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
        Err(original) if transaction.journal.committed => Err(original),
        Err(original) => Err(transaction.rollback(original)),
    }
}

pub(crate) fn remove_formulae(ctx: &Ctx, inputs: Vec<RemovalInput>) -> Result<(), OpError> {
    let mut transaction = RemovalTransaction {
        ctx,
        inputs,
        records: Vec::new(),
        staged: Vec::new(),
        unlinked: Vec::new(),
        trash_root: None,
        committed: false,
    };
    match transaction.apply() {
        Ok(()) => Ok(()),
        Err(original) if transaction.committed => Err(original),
        Err(original) => Err(transaction.rollback(original)),
    }
}

struct RemovalTransaction<'a> {
    ctx: &'a Ctx,
    inputs: Vec<RemovalInput>,
    records: Vec<RemovalRecord>,
    staged: Vec<StagedRemoval>,
    unlinked: Vec<UnlinkedKeg>,
    trash_root: Option<Utf8PathBuf>,
    committed: bool,
}

impl RemovalTransaction<'_> {
    fn apply(&mut self) -> Result<(), OpError> {
        for input in &self.inputs {
            let rack = self.ctx.env.cellar.join(input.name.name());
            for target in &input.targets {
                ensure_confined(target.keg.path(), &[&self.ctx.env.cellar])?;
                let metadata = fs::symlink_metadata(target.keg.path().as_std_path())
                    .map_err(|source| OpError::io("inspect", target.keg.path(), source))?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(OpError::InvalidState {
                        reason: format!(
                            "uninstall target is not a real directory: {}",
                            target.keg.path()
                        ),
                    });
                }
            }
            self.records.push(RemovalRecord {
                rack,
                rack_removed: false,
                opt: snapshot_symlink(self.ctx.env.prefix.join("opt").join(input.name.name()))?,
                linked: snapshot_symlink(self.ctx.env.linked.join(input.name.name()))?,
                pin: snapshot_symlink(self.ctx.env.pins.join(input.name.name()))?,
            });
        }

        let trash_root = create_trash_root(&self.ctx.env, "uninstall")?;
        self.trash_root = Some(trash_root.clone());
        let prefix = Prefix::new(self.ctx.env.clone());

        for (index, input) in self.inputs.iter().enumerate() {
            let formula_trash = trash_root.join(input.name.name());
            ensure_directory(&formula_trash, &trash_root, None)?;
            if input.remove_rack {
                remove_symlink_entry(&self.records[index].pin)?;
            }
            for target in &input.targets {
                if target.linked {
                    let report = unlink(&target.keg, &prefix)?;
                    self.unlinked.push(UnlinkedKeg {
                        keg: target.keg.clone(),
                        keg_only: report.removed.is_empty(),
                    });
                }
                if target.optlinked {
                    remove_symlink_entry(&self.records[index].opt)?;
                }
                let destination = formula_trash.join(target.keg.version().to_string());
                safe_trash_rename(&self.ctx.env, &trash_root, target.keg.path(), &destination)?;
                self.staged.push(StagedRemoval {
                    original: target.keg.path().to_path_buf(),
                    trash: destination,
                });
                maybe_fail_removal(input.name.name())?;
            }
            if input.remove_rack {
                remove_symlink_entry(&self.records[index].opt)?;
                remove_symlink_entry(&self.records[index].linked)?;
                remove_empty_dir_strict(&self.ctx.env, &self.records[index].rack)?;
                self.records[index].rack_removed = true;
            }
        }

        self.committed = true;
        let mut leftovers = Vec::new();
        for staged in &self.staged {
            if remove_trash_after_commit(&trash_root, &staged.trash).is_err()
                && path_entry_exists(&staged.trash)
            {
                leftovers.push(staged.trash.clone());
            }
        }
        remove_empty_trash_dirs(&trash_root);
        if leftovers.is_empty() {
            self.trash_root = None;
            return Ok(());
        }
        leftovers.sort();
        leftovers.dedup();
        Err(OpError::CleanupIncomplete {
            keg: self
                .staged
                .first()
                .map_or_else(|| trash_root.clone(), |staged| staged.original.clone()),
            leftovers,
        })
    }

    fn rollback(&mut self, original: OpError) -> OpError {
        let mut leftovers = Vec::new();
        for record in self
            .records
            .iter()
            .rev()
            .filter(|record| record.rack_removed)
        {
            if ensure_directory(&record.rack, &self.ctx.env.cellar, None).is_err() {
                leftovers.push(record.rack.clone());
            }
        }
        for staged in self.staged.iter().rev() {
            if path_entry_exists(&staged.trash)
                && self.trash_root.as_ref().is_none_or(|root| {
                    safe_trash_rename(&self.ctx.env, root, &staged.trash, &staged.original).is_err()
                })
            {
                leftovers.push(staged.trash.clone());
            }
        }

        let prefix = Prefix::new(self.ctx.env.clone());
        for old in self.unlinked.iter().rev() {
            match link(
                &old.keg,
                &prefix,
                LinkOptions {
                    keg_only: old.keg_only,
                    ..LinkOptions::default()
                },
            ) {
                Ok(report) => leftovers.extend(report.conflicts),
                Err(_) => leftovers.push(old.keg.path().to_path_buf()),
            }
        }
        for record in self.records.iter().rev() {
            for snapshot in [&record.pin, &record.linked, &record.opt] {
                if restore_symlink(&self.ctx.env, snapshot).is_err() {
                    leftovers.push(snapshot.path.clone());
                }
            }
        }

        if leftovers.is_empty()
            && let Some(root) = self.trash_root.as_ref()
            && path_entry_exists(root)
            && safe_remove_tree_within(root, &[root]).is_err()
        {
            leftovers.push(root.clone());
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

        let stage_root = create_stage(&rack)?;
        self.journal.stage_root = Some(stage_root.clone());

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
            self.input.tab.homebrew_version.as_deref(),
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
            let report = unlink(&linked, &Prefix::new(self.ctx.env.clone()))?;
            self.journal.old_unlinked = Some(UnlinkedKeg {
                keg: linked,
                keg_only: report.removed.is_empty(),
            });
            maybe_fail_install_after_unlink(self.name.name())?;
        }

        if let Some(target) = self.input.replacement.target.as_ref() {
            let backup = unique_backup_path(&rack, target.version().to_string().as_str());
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

        self.input.steps.execute(
            self.ctx,
            self.input.formula,
            self.final_keg.path(),
            &mut self.journal.steps,
        )?;

        self.journal.committed = true;
        let mut cleanup_leftovers = Vec::new();
        if let Some((_, backup)) = self.journal.backup.as_ref()
            && remove_after_commit(&self.ctx.env, backup).is_err()
            && path_entry_exists(backup)
        {
            cleanup_leftovers.push(backup.clone());
        }
        if let Some(step_root) = self.journal.steps.cleanup_path()
            && remove_after_commit(&self.ctx.env, step_root).is_err()
            && path_entry_exists(step_root)
        {
            cleanup_leftovers.push(step_root.to_path_buf());
        }
        if !cleanup_leftovers.is_empty() {
            cleanup_leftovers.sort();
            cleanup_leftovers.dedup();
            return Err(OpError::CleanupIncomplete {
                keg: self.final_keg.path().to_path_buf(),
                leftovers: cleanup_leftovers,
            });
        }
        self.journal.backup = None;
        self.journal.steps.clear();
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
        leftovers.extend(self.journal.steps.rollback(&self.ctx.env));

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
                &old.keg,
                &Prefix::new(self.ctx.env.clone()),
                LinkOptions {
                    keg_only: old.keg_only,
                    ..LinkOptions::default()
                },
            )
            .is_err()
        {
            leftovers.push(old.keg.path().to_path_buf());
            if let Ok(path) = Prefix::new(self.ctx.env.clone()).linked_path(old.keg.name()) {
                leftovers.push(path);
            }
            if let Ok(path) = Prefix::new(self.ctx.env.clone()).opt_path(old.keg.name()) {
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

fn create_stage(rack: &Utf8Path) -> Result<Utf8PathBuf, OpError> {
    loop {
        let id = next_stage_id(rack)?;
        let stage = rack.join(format!(".zapbrew-stage-{}-{id}", std::process::id()));
        match fs::create_dir(&stage) {
            Ok(()) => return Ok(stage),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(OpError::io(
                    "create exclusive staging directory",
                    stage,
                    source,
                ));
            }
        }
    }
}

fn unique_backup_path(rack: &Utf8Path, version: &str) -> Utf8PathBuf {
    loop {
        let id = TRANSACTION_ID.fetch_add(1, Ordering::Relaxed);
        let backup = rack.join(format!(
            ".zapbrew-backup-{version}-{}-{id}",
            std::process::id()
        ));
        if !path_entry_exists(&backup) {
            return backup;
        }
    }
}

fn next_stage_id(rack: &Utf8Path) -> Result<u64, OpError> {
    let mut hooks = TEST_HOOKS.lock().map_err(|_| OpError::InvalidState {
        reason: "transaction test-hook lock is poisoned".to_owned(),
    })?;
    if hooks
        .stage
        .as_ref()
        .is_some_and(|(candidate, _)| candidate == rack)
    {
        let (_, id) = hooks.stage.take().ok_or_else(|| OpError::InvalidState {
            reason: "transaction stage hook disappeared".to_owned(),
        })?;
        return Ok(id);
    }
    Ok(TRANSACTION_ID.fetch_add(1, Ordering::Relaxed))
}

fn remove_after_commit(env: &Env, path: &Utf8Path) -> Result<(), OpError> {
    inject_cleanup_failure(path)?;
    if path
        .file_name()
        .is_some_and(|name| name.starts_with(".zapbrew-step-journal"))
    {
        remove_tree_confined(env, path)
    } else {
        safe_remove_tree(env, path)
    }
}

fn remove_trash_after_commit(trash_root: &Utf8Path, path: &Utf8Path) -> Result<(), OpError> {
    inject_cleanup_failure(path)?;
    safe_remove_tree_within(path, &[trash_root])
}

fn inject_cleanup_failure(path: &Utf8Path) -> Result<(), OpError> {
    let should_fail = {
        let mut hooks = TEST_HOOKS.lock().map_err(|_| OpError::InvalidState {
            reason: "transaction test-hook lock is poisoned".to_owned(),
        })?;
        let formula = path
            .parent()
            .and_then(Utf8Path::file_name)
            .unwrap_or_default();
        if hooks.cleanup_failure_formula.as_deref() == Some(formula) {
            hooks.cleanup_failure_formula = None;
            true
        } else {
            false
        }
    };
    if !should_fail {
        return Ok(());
    }
    if let Ok(entries) = read_dir(path)
        && let Some(first) = entries.first()
    {
        let _ = if fs::symlink_metadata(first).is_ok_and(|metadata| metadata.file_type().is_dir()) {
            fs::remove_dir_all(first)
        } else {
            fs::remove_file(first)
        };
    }
    Err(OpError::io(
        "remove committed backup",
        path,
        io::Error::new(io::ErrorKind::PermissionDenied, "injected cleanup failure"),
    ))
}

pub(crate) fn arm_stage_collision(rack: Utf8PathBuf, id: u64) -> Result<Utf8PathBuf, OpError> {
    let candidate = rack.join(format!(".zapbrew-stage-{}-{id}", std::process::id()));
    TEST_HOOKS
        .lock()
        .map_err(|_| OpError::InvalidState {
            reason: "transaction test-hook lock is poisoned".to_owned(),
        })?
        .stage = Some((rack, id));
    Ok(candidate)
}

pub(crate) fn arm_cleanup_failure(formula: String) -> Result<(), OpError> {
    TEST_HOOKS
        .lock()
        .map_err(|_| OpError::InvalidState {
            reason: "transaction test-hook lock is poisoned".to_owned(),
        })?
        .cleanup_failure_formula = Some(formula);
    Ok(())
}

pub(crate) fn arm_install_failure_after_unlink(formula: String) -> Result<(), OpError> {
    TEST_HOOKS
        .lock()
        .map_err(|_| OpError::InvalidState {
            reason: "transaction test-hook lock is poisoned".to_owned(),
        })?
        .install_failure_after_unlink = Some(formula);
    Ok(())
}

pub(crate) fn arm_removal_failure_after(formula: String, staged: usize) -> Result<(), OpError> {
    if staged == 0 {
        return Err(OpError::InvalidState {
            reason: "removal failure hook requires at least one staged keg".to_owned(),
        });
    }
    TEST_HOOKS
        .lock()
        .map_err(|_| OpError::InvalidState {
            reason: "transaction test-hook lock is poisoned".to_owned(),
        })?
        .removal_failure_after = Some((formula, staged));
    Ok(())
}

pub(crate) fn cleanup_replaced_kegs(
    env: &Env,
    active_keg: &Utf8Path,
    name: &FormulaName,
    kegs: &[Keg],
) -> Result<(), OpError> {
    if kegs.is_empty() {
        return Ok(());
    }
    let trash_root = create_trash_root(env, "upgrade")?;
    let formula_trash = trash_root.join(name.name());
    ensure_directory(&formula_trash, &trash_root, None)?;
    let mut staged: Vec<StagedRemoval> = Vec::with_capacity(kegs.len());
    for keg in kegs {
        let destination = formula_trash.join(keg.version().to_string());
        if let Err(original) = safe_trash_rename(env, &trash_root, keg.path(), &destination) {
            let mut leftovers = Vec::new();
            for entry in staged.iter().rev() {
                if safe_trash_rename(env, &trash_root, &entry.trash, &entry.original).is_err() {
                    leftovers.push(entry.trash.clone());
                }
            }
            if leftovers.is_empty() {
                remove_empty_trash_dirs(&trash_root);
                return Err(original);
            }
            leftovers.sort();
            leftovers.dedup();
            return Err(OpError::CleanupIncomplete {
                keg: active_keg.to_path_buf(),
                leftovers,
            });
        }
        staged.push(StagedRemoval {
            original: keg.path().to_path_buf(),
            trash: destination,
        });
    }
    let mut leftovers = Vec::new();
    for entry in &staged {
        if remove_trash_after_commit(&trash_root, &entry.trash).is_err()
            && path_entry_exists(&entry.trash)
        {
            leftovers.push(entry.trash.clone());
        }
    }
    remove_empty_trash_dirs(&trash_root);
    if leftovers.is_empty() {
        Ok(())
    } else {
        leftovers.sort();
        leftovers.dedup();
        Err(OpError::CleanupIncomplete {
            keg: active_keg.to_path_buf(),
            leftovers,
        })
    }
}

fn maybe_fail_install_after_unlink(formula: &str) -> Result<(), OpError> {
    let mut hooks = TEST_HOOKS.lock().map_err(|_| OpError::InvalidState {
        reason: "transaction test-hook lock is poisoned".to_owned(),
    })?;
    if hooks.install_failure_after_unlink.as_deref() != Some(formula) {
        return Ok(());
    }
    hooks.install_failure_after_unlink = None;
    Err(OpError::InvalidState {
        reason: "injected install failure after unlink".to_owned(),
    })
}

fn maybe_fail_removal(formula: &str) -> Result<(), OpError> {
    let mut hooks = TEST_HOOKS.lock().map_err(|_| OpError::InvalidState {
        reason: "transaction test-hook lock is poisoned".to_owned(),
    })?;
    let Some((candidate, remaining)) = hooks.removal_failure_after.as_mut() else {
        return Ok(());
    };
    if candidate != formula {
        return Ok(());
    }
    *remaining -= 1;
    if *remaining != 0 {
        return Ok(());
    }
    hooks.removal_failure_after = None;
    Err(OpError::InvalidState {
        reason: "injected pre-commit removal failure".to_owned(),
    })
}

fn create_trash_root(env: &Env, purpose: &str) -> Result<Utf8PathBuf, OpError> {
    let parent = env.cellar.parent().ok_or_else(|| OpError::InvalidState {
        reason: format!("cellar has no parent: {}", env.cellar),
    })?;
    ensure_existing_directory(parent)?;
    loop {
        let id = TRANSACTION_ID.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".zapbrew-cellar-{purpose}-trash-{}-{id}",
            std::process::id()
        ));
        match fs::create_dir(path.as_std_path()) {
            Ok(()) => return Ok(path),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(OpError::io(
                    "create exclusive trash directory",
                    path,
                    source,
                ));
            }
        }
    }
}

fn snapshot_symlink(path: Utf8PathBuf) -> Result<SymlinkSnapshot, OpError> {
    let target = match fs::symlink_metadata(path.as_std_path()) {
        Ok(metadata) if metadata.file_type().is_symlink() => Some(
            fs::read_link(path.as_std_path())
                .map_err(|source| OpError::io("read symlink", path.clone(), source))?,
        ),
        Ok(_) => {
            return Err(OpError::InvalidState {
                reason: format!("formula record is not a symlink: {path}"),
            });
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => None,
        Err(source) => return Err(OpError::io("inspect", path, source)),
    };
    Ok(SymlinkSnapshot { path, target })
}

fn remove_symlink_entry(snapshot: &SymlinkSnapshot) -> Result<(), OpError> {
    match fs::symlink_metadata(snapshot.path.as_std_path()) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::remove_file(snapshot.path.as_std_path())
                .map_err(|source| OpError::io("remove symlink", snapshot.path.clone(), source))
        }
        Ok(_) => Err(OpError::InvalidState {
            reason: format!("formula record is not a symlink: {}", snapshot.path),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(OpError::io("inspect", snapshot.path.clone(), source)),
    }
}

fn restore_symlink(env: &Env, snapshot: &SymlinkSnapshot) -> Result<(), OpError> {
    remove_symlink_entry(snapshot)?;
    let Some(target) = snapshot.target.as_ref() else {
        return Ok(());
    };
    let parent = snapshot
        .path
        .parent()
        .ok_or_else(|| OpError::InvalidState {
            reason: format!("symlink path has no parent: {}", snapshot.path),
        })?;
    ensure_directory(parent, &env.prefix, None)?;
    symlink(target, snapshot.path.as_std_path())
        .map_err(|source| OpError::io("restore symlink", snapshot.path.clone(), source))
}

fn remove_empty_dir_strict(env: &Env, path: &Utf8Path) -> Result<(), OpError> {
    ensure_confined(path, &[&env.cellar])?;
    let metadata = fs::symlink_metadata(path.as_std_path())
        .map_err(|source| OpError::io("inspect", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(OpError::InvalidState {
            reason: format!("rack is not a real directory: {path}"),
        });
    }
    fs::remove_dir(path.as_std_path()).map_err(|source| OpError::io("remove rack", path, source))
}

fn remove_empty_trash_dirs(root: &Utf8Path) {
    if let Ok(formulae) = read_dir(root) {
        for formula in formulae {
            let _ = fs::remove_dir(formula.as_std_path());
        }
    }
    let _ = fs::remove_dir(root.as_std_path());
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
    safe_rename_within(source, destination, &[&env.cellar])
}

fn safe_trash_rename(
    env: &Env,
    trash_root: &Utf8Path,
    source: &Utf8Path,
    destination: &Utf8Path,
) -> Result<(), OpError> {
    safe_rename_within(source, destination, &[&env.cellar, trash_root])
}

fn safe_rename_within(
    source: &Utf8Path,
    destination: &Utf8Path,
    roots: &[&Utf8Path],
) -> Result<(), OpError> {
    ensure_confined(source, roots)?;
    ensure_confined(destination, roots)?;
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
    safe_remove_tree_within(path, &[&env.cellar])
}

fn safe_remove_tree_within(path: &Utf8Path, roots: &[&Utf8Path]) -> Result<(), OpError> {
    ensure_confined(path, roots)?;
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
