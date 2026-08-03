use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_prefix::{Env, Keg, LockGuard, PrefixError, Rack};

use crate::size::disk_usage_readable;
use crate::state::{InstalledState, scan_selected};
use crate::transaction::{RemovalInput, RemovalKeg, acquire_formula_locks, remove_formulae};
use crate::{Ctx, OpError};

const SECONDS_PER_DAY: i64 = 86_400;
const PREFIX_LINK_ROOTS: &[&str] = &[
    "Frameworks",
    "bin",
    "etc",
    "include",
    "lib",
    "opt",
    "sbin",
    "share",
    "var/homebrew/linked",
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub dry_run: bool,
    pub scrub: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CandidateKind {
    Formula,
    Cache,
    PrefixSymlink,
    PrefixDirectory,
    Lock,
}

#[derive(Debug, Clone)]
struct Candidate {
    path: Utf8PathBuf,
    size: u64,
    kind: CandidateKind,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    ensure_real_root(&ctx.env.prefix)?;
    ensure_real_root(&ctx.env.cellar)?;
    ensure_real_root(&ctx.env.cache)?;
    ensure_real_root(&ctx.env.locks)?;
    ensure_no_symlink_components(&ctx.env.prefix, &ctx.env.locks)?;

    let explicitly_named = !args.names.is_empty();
    let mut names = resolve_racks(ctx, &args.names)?;
    retain_cleanable(ctx, &mut names, explicitly_named);

    let formula_locks = if args.dry_run {
        None
    } else {
        Some(acquire_formula_locks(&ctx.env, &names)?)
    };
    let state = scan_selected(&ctx.env, &names)?;
    let mut candidates = BTreeMap::new();
    collect_formula_candidates(ctx, &state, &mut candidates);
    collect_cache_candidates(
        ctx,
        &state,
        explicitly_named.then_some(&names),
        args.scrub,
        &mut candidates,
    )?;
    collect_prefix_candidates(&ctx.env, &mut candidates)?;

    if args.dry_run {
        collect_lock_candidates(&ctx.env, &mut candidates)?;
        for candidate in candidates.values() {
            ctx.reporter.print(&format!(
                "Would remove: {} ({})",
                candidate.path,
                disk_usage_readable(candidate.size)
            ));
        }
        report_total(
            ctx,
            candidates.values().map(|candidate| candidate.size).sum(),
            true,
        );
        return Ok(());
    }

    let mut failed = Vec::new();
    let mut removed_size = 0_u64;
    apply_formula_candidates(ctx, &state, &candidates, &mut removed_size, &mut failed);
    apply_file_candidates(
        &ctx.env,
        &candidates,
        CandidateKind::Cache,
        &mut removed_size,
        &mut failed,
    );
    apply_file_candidates(
        &ctx.env,
        &candidates,
        CandidateKind::PrefixSymlink,
        &mut removed_size,
        &mut failed,
    );
    apply_directory_candidates(&ctx.env, &candidates, &mut failed);

    if let Some(guards) = formula_locks {
        for guard in &guards {
            match fs::remove_file(guard.path()) {
                Ok(()) => {}
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(_) => failed.push(guard.path().to_path_buf()),
            }
        }
        drop(guards);
    }
    cleanup_stale_locks(&ctx.env, &mut failed, &mut removed_size)?;

    failed.sort();
    failed.dedup();
    if !failed.is_empty() {
        return Err(OpError::CleanupIncomplete {
            keg: ctx.env.prefix.clone(),
            leftovers: failed,
        });
    }

    fs::create_dir_all(&ctx.env.cache)
        .map_err(|source| OpError::io("create cache", ctx.env.cache.clone(), source))?;
    let marker = ctx.env.cache.join(".cleaned");
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&marker)
        .and_then(|file| file.set_modified(SystemTime::now()))
        .map_err(|source| OpError::io("touch", marker, source))?;
    report_total(ctx, removed_size, false);
    Ok(())
}

fn resolve_racks(ctx: &Ctx, requested: &[String]) -> Result<BTreeSet<String>, OpError> {
    if requested.is_empty() {
        if !present_real_directory(&ctx.env.cellar)? {
            return Ok(BTreeSet::new());
        }
        return Rack::all(&ctx.env.cellar)
            .map(|racks| {
                racks
                    .into_iter()
                    .map(|rack| rack.name().to_owned())
                    .collect()
            })
            .map_err(OpError::from);
    }

    requested
        .iter()
        .map(|name| {
            ctx.catalog
                .get(name)
                .map(|formula| formula.name.clone())
                .ok_or_else(|| OpError::MissingFormula { name: name.clone() })
        })
        .collect()
}

fn retain_cleanable(ctx: &Ctx, names: &mut BTreeSet<String>, explicitly_named: bool) {
    names.retain(|name| {
        let aliases = ctx
            .catalog
            .get(name)
            .map_or(&[][..], |formula| formula.aliases.as_slice());
        let excluded = ctx
            .env
            .no_cleanup_formulae
            .iter()
            .any(|excluded| excluded == name || aliases.iter().any(|alias| alias == excluded));
        if excluded && explicitly_named {
            ctx.reporter.onoe(&format!(
                "Refusing to clean {name} because it is listed in HOMEBREW_NO_CLEANUP_FORMULAE!"
            ));
        }
        !excluded
    });
}

fn collect_formula_candidates(
    ctx: &Ctx,
    state: &InstalledState,
    candidates: &mut BTreeMap<Utf8PathBuf, Candidate>,
) {
    for installed in state.iter() {
        let Some(formula) = ctx.catalog.get(installed.name().name()) else {
            ctx.reporter.opoo(&format!(
                "Skipping {}: most recent version is unavailable in the catalog",
                installed.name()
            ));
            continue;
        };
        if !installed
            .kegs()
            .iter()
            .any(|keg| keg.version() == &formula.pkg_version)
        {
            ctx.reporter.opoo(&format!(
                "Skipping {}: most recent version {} not installed",
                formula.full_name, formula.pkg_version
            ));
            continue;
        }

        for keg in installed.kegs() {
            let installed_scheme = keg.tab().source.versions.version_scheme;
            let old = formula.version_scheme > installed_scheme
                || (formula.version_scheme == installed_scheme
                    && formula.pkg_version > *keg.version());
            if !old {
                continue;
            }
            if keg.is_linked() {
                ctx.reporter.opoo(&format!(
                    "Skipping (old) {} due to it being linked",
                    keg.path()
                ));
                continue;
            }
            if keg.is_pinned() {
                ctx.reporter.opoo(&format!(
                    "Skipping (old) {} due to it being pinned",
                    keg.path()
                ));
                continue;
            }
            candidates.insert(
                keg.path().to_path_buf(),
                Candidate {
                    path: keg.path().to_path_buf(),
                    size: keg.size(),
                    kind: CandidateKind::Formula,
                },
            );
        }
    }
}

fn collect_cache_candidates(
    ctx: &Ctx,
    state: &InstalledState,
    scrub_scope: Option<&BTreeSet<String>>,
    scrub: bool,
    candidates: &mut BTreeMap<Utf8PathBuf, Candidate>,
) -> Result<(), OpError> {
    if !present_real_directory(&ctx.env.cache)? {
        return Ok(());
    }
    let (referenced, invalid_aliases) = cache_aliases(&ctx.env)?;
    for alias in invalid_aliases {
        insert_candidate(candidates, alias, CandidateKind::Cache)?;
    }

    let mut entries = Vec::new();
    walk_cache(&ctx.env.cache, &ctx.env.cache, &mut entries)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(i64::MAX, |duration| duration.as_secs() as i64);
    for path in entries {
        if path == ctx.env.cache.join(".cleaned") || candidates.contains_key(&path) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => return Err(OpError::io("inspect", path, source)),
        };
        let incomplete = path
            .file_name()
            .is_some_and(|name| name.ends_with(".incomplete"));
        let aged = !metadata.is_dir()
            && !referenced.contains(&path)
            && older_than(
                metadata.mtime(),
                metadata.ctime(),
                now,
                ctx.env.cleanup_max_age_days,
            );
        let stale_bottle =
            scrub && !metadata.is_dir() && stale_bottle(ctx, state, scrub_scope, &path);
        if incomplete || aged || stale_bottle {
            insert_candidate(candidates, path, CandidateKind::Cache)?;
        }
    }
    Ok(())
}

fn cache_aliases(env: &Env) -> Result<(BTreeSet<Utf8PathBuf>, Vec<Utf8PathBuf>), OpError> {
    let mut referenced = BTreeSet::new();
    let mut invalid = Vec::new();
    let entries = fs::read_dir(&env.cache)
        .map_err(|source| OpError::io("read directory", env.cache.clone(), source))?;
    for entry in entries {
        let entry =
            entry.map_err(|source| OpError::io("read directory", env.cache.clone(), source))?;
        let path =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("cache path is not UTF-8: {}", path.display()),
            })?;
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| OpError::io("inspect", path.clone(), source))?;
        if !metadata.file_type().is_symlink() {
            continue;
        }
        let target = fs::read_link(&path)
            .map_err(|source| OpError::io("read symlink", path.clone(), source))?;
        let Some(target) = confined_symlink_target(&env.cache, &path, &target) else {
            invalid.push(path);
            continue;
        };
        match fs::symlink_metadata(&target) {
            Ok(_) => {
                referenced.insert(path.clone());
                referenced.insert(target);
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => invalid.push(path),
            Err(source) => return Err(OpError::io("inspect", target, source)),
        }
    }
    Ok((referenced, invalid))
}

fn stale_bottle(
    ctx: &Ctx,
    state: &InstalledState,
    scope: Option<&BTreeSet<String>>,
    path: &Utf8Path,
) -> bool {
    let Some(file_name) = path.file_name() else {
        return false;
    };
    let basename = strip_download_hash(file_name);
    let Some((name, version_and_tag)) = basename.split_once("--") else {
        return false;
    };
    if !version_and_tag.contains(".bottle") || !version_and_tag.ends_with(".tar.gz") {
        return false;
    }
    if scope.is_some_and(|names| !names.contains(name)) {
        return false;
    }
    let Some(formula) = ctx.catalog.get(name) else {
        return false;
    };
    let mut kept = BTreeSet::from([formula.pkg_version.to_string()]);
    if let Some(installed) = state.formula(name) {
        kept.extend(installed.kegs().iter().map(|keg| keg.version().to_string()));
    }
    !kept
        .iter()
        .any(|version| version_and_tag.starts_with(&format!("{version}.")))
}

fn strip_download_hash(file_name: &str) -> &str {
    let Some((prefix, rest)) = file_name.split_once("--") else {
        return file_name;
    };
    if prefix.len() == 64 && prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        rest
    } else {
        file_name
    }
}

fn collect_prefix_candidates(
    env: &Env,
    candidates: &mut BTreeMap<Utf8PathBuf, Candidate>,
) -> Result<(), OpError> {
    for path in broken_prefix_symlinks(env)? {
        insert_candidate(candidates, path, CandidateKind::PrefixSymlink)?;
    }
    for root in prefix_link_roots(env) {
        if fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.is_dir()) {
            collect_empty_directories(&root, &root, candidates)?;
        }
    }
    Ok(())
}

pub(crate) fn broken_prefix_symlinks(env: &Env) -> Result<Vec<Utf8PathBuf>, OpError> {
    let mut broken = Vec::new();
    for root in prefix_link_roots(env) {
        if real_directory_below(&env.prefix, &root)? {
            collect_broken_symlinks(&root, &mut broken)?;
        }
    }
    broken.sort();
    broken.dedup();
    Ok(broken)
}

pub(crate) fn cache_incomplete_entries(env: &Env) -> Result<Vec<Utf8PathBuf>, OpError> {
    if !present_real_directory(&env.cache)? {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    walk_cache(&env.cache, &env.cache, &mut entries)?;
    entries.retain(|path| {
        path.file_name()
            .is_some_and(|name| name.ends_with(".incomplete"))
    });
    entries.sort();
    Ok(entries)
}

fn prefix_link_roots(env: &Env) -> Vec<Utf8PathBuf> {
    PREFIX_LINK_ROOTS
        .iter()
        .map(|relative| env.prefix.join(relative))
        .collect()
}

fn collect_broken_symlinks(dir: &Utf8Path, output: &mut Vec<Utf8PathBuf>) -> Result<(), OpError> {
    for path in sorted_children(dir)? {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| OpError::io("inspect", path.clone(), source))?;
        if metadata.file_type().is_symlink() {
            if fs::metadata(&path).is_err_and(|source| source.kind() == io::ErrorKind::NotFound) {
                output.push(path);
            }
        } else if metadata.is_dir() {
            collect_broken_symlinks(&path, output)?;
        }
    }
    Ok(())
}

fn collect_empty_directories(
    root: &Utf8Path,
    dir: &Utf8Path,
    candidates: &mut BTreeMap<Utf8PathBuf, Candidate>,
) -> Result<bool, OpError> {
    let mut empty_after_cleanup = true;
    for path in sorted_children(dir)? {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| OpError::io("inspect", path.clone(), source))?;
        let removable = if metadata.file_type().is_symlink() {
            candidates
                .get(&path)
                .is_some_and(|candidate| candidate.kind == CandidateKind::PrefixSymlink)
        } else if metadata.is_dir() {
            collect_empty_directories(root, &path, candidates)?
        } else {
            false
        };
        empty_after_cleanup &= removable;
    }
    if empty_after_cleanup && dir != root {
        candidates.insert(
            dir.to_path_buf(),
            Candidate {
                path: dir.to_path_buf(),
                size: 0,
                kind: CandidateKind::PrefixDirectory,
            },
        );
        Ok(true)
    } else {
        Ok(false)
    }
}

fn walk_cache(
    root: &Utf8Path,
    dir: &Utf8Path,
    output: &mut Vec<Utf8PathBuf>,
) -> Result<(), OpError> {
    for path in sorted_children(dir)? {
        if !path.starts_with(root) {
            return Err(OpError::InvalidState {
                reason: format!("cache scan escaped {root}: {path}"),
            });
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| OpError::io("inspect", path.clone(), source))?;
        output.push(path.clone());
        if metadata.is_dir()
            && !path
                .file_name()
                .is_some_and(|name| name.ends_with(".incomplete"))
        {
            walk_cache(root, &path, output)?;
        }
    }
    Ok(())
}

fn sorted_children(dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, OpError> {
    let entries = fs::read_dir(dir)
        .map_err(|source| OpError::io("read directory", dir.to_path_buf(), source))?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|source| OpError::io("read directory", dir.to_path_buf(), source))?;
        paths.push(Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| {
            OpError::InvalidState {
                reason: format!("filesystem path is not UTF-8: {}", path.display()),
            }
        })?);
    }
    paths.sort();
    Ok(paths)
}

fn insert_candidate(
    candidates: &mut BTreeMap<Utf8PathBuf, Candidate>,
    path: Utf8PathBuf,
    kind: CandidateKind,
) -> Result<(), OpError> {
    let size = entry_size(&path)?;
    candidates
        .entry(path.clone())
        .or_insert(Candidate { path, size, kind });
    Ok(())
}

fn entry_size(path: &Utf8Path) -> Result<u64, OpError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(OpError::io("inspect", path.to_path_buf(), source)),
    };
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    let mut size = 0_u64;
    for child in sorted_children(path)? {
        size = size.saturating_add(entry_size(&child)?);
    }
    Ok(size)
}

pub(crate) fn older_than(mtime: i64, ctime: i64, now: i64, days: u64) -> bool {
    let seconds = i64::try_from(days)
        .map_or(i64::MAX, |days| days)
        .saturating_mul(SECONDS_PER_DAY);
    let cutoff = now.saturating_sub(seconds);
    mtime < cutoff && ctime < cutoff
}

fn apply_formula_candidates(
    ctx: &Ctx,
    state: &InstalledState,
    candidates: &BTreeMap<Utf8PathBuf, Candidate>,
    removed_size: &mut u64,
    failed: &mut Vec<Utf8PathBuf>,
) {
    for installed in state.iter() {
        let selected = installed
            .kegs()
            .iter()
            .filter(|keg| {
                candidates
                    .get(keg.path())
                    .is_some_and(|candidate| candidate.kind == CandidateKind::Formula)
            })
            .collect::<Vec<_>>();
        if selected.is_empty() {
            continue;
        }
        let targets = selected
            .iter()
            .filter_map(|keg| {
                Keg::new(
                    &ctx.env.cellar,
                    installed.name().clone(),
                    keg.version().clone(),
                )
                .ok()
                .map(|keg_path| RemovalKeg {
                    keg: keg_path,
                    linked: keg.is_linked(),
                    optlinked: keg.is_optlinked(),
                })
            })
            .collect::<Vec<_>>();
        if targets.len() != selected.len() {
            failed.extend(selected.iter().map(|keg| keg.path().to_path_buf()));
            continue;
        }
        let input = RemovalInput {
            name: installed.name().clone(),
            targets,
            remove_rack: selected.len() == installed.kegs().len(),
        };
        match remove_formulae(ctx, vec![input]) {
            Ok(()) => {
                *removed_size =
                    removed_size.saturating_add(selected.iter().map(|keg| keg.size()).sum());
            }
            Err(OpError::CleanupIncomplete { leftovers, .. }) => failed.extend(leftovers),
            Err(_) => failed.extend(selected.iter().map(|keg| keg.path().to_path_buf())),
        }
    }
}

fn apply_file_candidates(
    env: &Env,
    candidates: &BTreeMap<Utf8PathBuf, Candidate>,
    kind: CandidateKind,
    removed_size: &mut u64,
    failed: &mut Vec<Utf8PathBuf>,
) {
    for candidate in candidates
        .values()
        .filter(|candidate| candidate.kind == kind)
    {
        let root = if kind == CandidateKind::Cache {
            &env.cache
        } else {
            &env.prefix
        };
        match remove_entry(candidate.path.as_path(), root) {
            Ok(true) => *removed_size = removed_size.saturating_add(candidate.size),
            Ok(false) => {}
            Err(_) => failed.push(candidate.path.clone()),
        }
    }
}

fn apply_directory_candidates(
    env: &Env,
    candidates: &BTreeMap<Utf8PathBuf, Candidate>,
    failed: &mut Vec<Utf8PathBuf>,
) {
    let mut directories = candidates
        .values()
        .filter(|candidate| candidate.kind == CandidateKind::PrefixDirectory)
        .collect::<Vec<_>>();
    directories.sort_by(|left, right| {
        right
            .path
            .components()
            .count()
            .cmp(&left.path.components().count())
            .then_with(|| left.path.cmp(&right.path))
    });
    for candidate in directories {
        if !candidate.path.starts_with(&env.prefix) || candidate.path == env.prefix {
            failed.push(candidate.path.clone());
            continue;
        }
        match fs::remove_dir(&candidate.path) {
            Ok(()) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(_) => failed.push(candidate.path.clone()),
        }
    }
}

fn remove_entry(path: &Utf8Path, root: &Utf8Path) -> Result<bool, OpError> {
    if !path.starts_with(root) || path == root {
        return Err(OpError::InvalidState {
            reason: format!("cleanup target escaped {root}: {path}"),
        });
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(OpError::io("inspect", path.to_path_buf(), source)),
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)
            .map_err(|source| OpError::io("remove", path.to_path_buf(), source))?;
        return Ok(true);
    }
    for child in sorted_children(path)? {
        remove_entry(&child, root)?;
    }
    fs::remove_dir(path).map_err(|source| OpError::io("remove", path.to_path_buf(), source))?;
    Ok(true)
}

fn collect_lock_candidates(
    env: &Env,
    candidates: &mut BTreeMap<Utf8PathBuf, Candidate>,
) -> Result<(), OpError> {
    if !present_real_directory(&env.locks)? {
        return Ok(());
    }
    for path in sorted_children(&env.locks)? {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| OpError::io("inspect", path.clone(), source))?;
        if !metadata.is_file() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        match LockGuard::acquire(&env.locks, name) {
            Ok(_guard) => {
                candidates.insert(
                    path.clone(),
                    Candidate {
                        path,
                        size: metadata.len(),
                        kind: CandidateKind::Lock,
                    },
                );
            }
            Err(PrefixError::LockBusy { .. }) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn cleanup_stale_locks(
    env: &Env,
    failed: &mut Vec<Utf8PathBuf>,
    removed_size: &mut u64,
) -> Result<(), OpError> {
    if !present_real_directory(&env.locks)? {
        return Ok(());
    }
    for path in sorted_children(&env.locks)? {
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                failed.push(path);
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        match LockGuard::acquire(&env.locks, name) {
            Ok(guard) => match fs::remove_file(guard.path()) {
                Ok(()) => *removed_size = removed_size.saturating_add(metadata.len()),
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(_) => failed.push(path),
            },
            Err(PrefixError::LockBusy { .. }) => {}
            Err(_) => failed.push(path),
        }
    }
    Ok(())
}

fn real_directory_below(root: &Utf8Path, path: &Utf8Path) -> Result<bool, OpError> {
    let relative = path.strip_prefix(root).map_err(|_| OpError::InvalidState {
        reason: format!("path escaped {root}: {path}"),
    })?;
    if !present_real_directory(root)? {
        return Ok(false);
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(OpError::io("inspect", current, source)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn ensure_real_root(path: &Utf8Path) -> Result<(), OpError> {
    present_real_directory(path).map(|_| ())
}

fn present_real_directory(path: &Utf8Path) -> Result<bool, OpError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(OpError::InvalidState {
                reason: format!("cleanup root is not a real directory: {path}"),
            })
        }
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(OpError::io("inspect", path.to_path_buf(), source)),
    }
}

fn ensure_no_symlink_components(root: &Utf8Path, path: &Utf8Path) -> Result<(), OpError> {
    let relative = path.strip_prefix(root).map_err(|_| OpError::InvalidState {
        reason: format!("path escaped {root}: {path}"),
    })?;
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|source| OpError::io("inspect", root.to_path_buf(), source))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(OpError::InvalidState {
            reason: format!("cleanup root is not a real directory: {root}"),
        });
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_str());
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(OpError::io("inspect", current, source)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(OpError::InvalidState {
                reason: format!("cleanup path has a non-directory component: {current}"),
            });
        }
    }
    Ok(())
}

fn confined_symlink_target(root: &Utf8Path, link: &Utf8Path, target: &Path) -> Option<Utf8PathBuf> {
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent()?.as_std_path().join(target)
    };
    let normalized = normalize(&joined)?;
    let normalized = Utf8PathBuf::from_path_buf(normalized).ok()?;
    normalized.starts_with(root).then_some(normalized)
}

fn normalize(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    Some(normalized)
}

fn report_total(ctx: &Ctx, bytes: u64, dry_run: bool) {
    if bytes == 0 {
        return;
    }
    if dry_run {
        ctx.reporter.ohai(&format!(
            "This operation would free approximately {} of disk space.",
            disk_usage_readable(bytes)
        ));
    } else {
        ctx.reporter.ohai(&format!(
            "This operation has freed approximately {} of disk space.",
            disk_usage_readable(bytes)
        ));
    }
}
