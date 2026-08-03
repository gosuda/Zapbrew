//! Symlink an installed keg into the Homebrew prefix and remove those links.
//!
//! The directory strategy tables (which top-level keg directories are walked,
//! and which relative paths are symlinked whole, materialized as real
//! directories, or skipped) mirror Homebrew's `keg.rb` exactly; see Appendix E
//! of the rewrite plan. Every mutating step is planned read-only first so a
//! conflict aborts before a single symlink is written.

use std::collections::BTreeSet;
use std::fs;
use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_prefix::{Keg, Prefix};

use crate::error::PourError;
use crate::types::{LinkOptions, LinkReport, UnlinkReport};

/// Top-level keg directories walked when linking, in Homebrew's order.
///
/// `var` is deliberately absent: its contents are installed by the etc/var
/// skeleton copy, never symlinked. `Frameworks` only exists in macOS bottles
/// and is silently skipped when the directory is absent.
const LINK_DIRS: [&str; 7] = [
    "etc",
    "bin",
    "sbin",
    "include",
    "share",
    "lib",
    "Frameworks",
];

/// Top-level keg directories walked when unlinking (`keg_link_directories`).
const UNLINK_DIRS: [&str; 7] = ["bin", "etc", "include", "lib", "sbin", "share", "var"];

/// Prefix subdirectories that must survive pruning (`must_exist_subdirectories`).
const MUST_EXIST_TOP: [&str; 7] = ["bin", "etc", "include", "lib", "sbin", "share", "opt"];

/// Per-directory link strategy for a relative path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Strategy {
    /// Skip this file (do not link).
    SkipFile,
    /// Skip this directory (do not descend, do not link).
    SkipDir,
    /// Materialize a real directory in the prefix and descend into it.
    Mkpath,
    /// Symlink the whole entry into the prefix.
    Link,
    /// Symlink an info file (and, in Homebrew, run `install-info`).
    Info,
}

/// What already occupies a planned destination.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DstState {
    /// Nothing exists at the destination.
    Absent,
    /// A symlink whose target does not resolve.
    BrokenSymlink,
    /// A symlink already resolving into this keg.
    SymlinkToKeg,
    /// A symlink resolving to a real directory under the cellar (mergeable).
    SymlinkToCellarDir(Utf8PathBuf),
    /// A real directory.
    RealDir,
    /// A real file, or a symlink to something outside this keg (a conflict).
    Conflict,
}

/// A pre-link cleanup step for an occupied destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pre {
    None,
    RemoveSymlink,
    Backup,
}

/// A validated, ordered mutation planned before any writes occur.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    Mkdir(Utf8PathBuf),
    RemoveSymlinkThenMkdir(Utf8PathBuf),
    BackupThenMkdir(Utf8PathBuf),
    Expand {
        dst: Utf8PathBuf,
        real: Utf8PathBuf,
    },
    Symlink {
        dst: Utf8PathBuf,
        target: Utf8PathBuf,
        pre: Pre,
    },
}

/// Symlink every eligible file in `keg` into the prefix and record the opt and
/// linked keg entries.
///
/// The full plan and every conflict are computed before any mutation. When a
/// conflict is found and `overwrite` is not set, nothing is written and the
/// conflicts are returned. A keg-only formula (without `force`) skips prefix
/// file links but still gets its opt and linked records.
pub fn link(keg: &Keg, prefix: &Prefix, options: LinkOptions) -> Result<LinkReport, PourError> {
    let keg_root = keg.path();
    let prefix_path = prefix.path();
    let cellar = prefix.cellar();
    let skip_file_links = options.keg_only && !options.force;

    let mut planner = Planner {
        keg_root,
        prefix_path,
        cellar,
        overwrite: options.overwrite,
        ops: Vec::new(),
        conflicts: Vec::new(),
        would_link: Vec::new(),
    };

    if !skip_file_links {
        for dir in LINK_DIRS {
            let root = keg_root.join(dir);
            if path_exists(&root) {
                planner.walk(dir, &root)?;
            }
        }
    }

    let Planner {
        ops,
        conflicts,
        would_link,
        ..
    } = planner;

    // Dry run: report what would happen, mutate nothing (no file links, no
    // opt/linked records). With overwrite, include destinations that
    // BackupThenMkdir / Symlink{pre: Backup} would move under Backup/.
    if options.dry_run {
        return Ok(LinkReport {
            linked: would_link,
            conflicts,
            backups: planned_backups(&ops),
        });
    }

    // Preflight gate: a conflict without --overwrite aborts before any write.
    if !conflicts.is_empty() {
        return Ok(LinkReport {
            linked: Vec::new(),
            conflicts,
            backups: Vec::new(),
        });
    }

    let backup_dir = prefix.env().cache.join("Backup");
    let mut linked = Vec::new();
    let mut backups = Vec::new();

    for dir in MUST_EXIST_TOP {
        create_dir_all(&prefix_path.join(dir))?;
    }

    for op in ops {
        apply_op(op, prefix_path, &backup_dir, &mut linked, &mut backups)?;
    }

    // opt and linked keg records are always written (even for keg-only).
    write_record(&prefix.opt_path(keg.name())?, keg_root)?;
    write_record(&prefix.linked_path(keg.name())?, keg_root)?;

    Ok(LinkReport {
        linked,
        conflicts: Vec::new(),
        backups,
    })
}

/// Remove exactly the prefix symlinks that resolve into `keg`, prune the empty
/// directories that ownership created, and drop the linked keg record. The opt
/// record is retained until uninstall.
pub fn unlink(keg: &Keg, prefix: &Prefix) -> Result<UnlinkReport, PourError> {
    let keg_root = keg.path();
    let prefix_path = prefix.path();

    let mut removed = Vec::new();
    let mut owned_dirs: BTreeSet<Utf8PathBuf> = BTreeSet::new();

    for dir in UNLINK_DIRS {
        let root = keg_root.join(dir);
        if path_exists(&root) {
            walk_unlink(&root, keg_root, prefix_path, &mut removed, &mut owned_dirs)?;
        }
    }

    // Drop the linked keg record when it still points at this keg; keep opt.
    let linked_record = prefix.linked_path(keg.name())?;
    if is_symlink(&linked_record) && symlink_resolves_to(&linked_record, keg_root) {
        remove_file(&linked_record)?;
    }

    let must_exist = must_exist_set(prefix);
    let mut prune: Vec<Utf8PathBuf> = owned_dirs.into_iter().collect();
    // Deepest first so children are gone before their parents are tried.
    prune.sort_by(|a, b| {
        b.components()
            .count()
            .cmp(&a.components().count())
            .then_with(|| b.cmp(a))
    });

    let mut pruned = Vec::new();
    for dir in prune {
        if must_exist.contains(&dir) {
            continue;
        }
        if rmdir_if_possible(&dir)? {
            pruned.push(dir);
        }
    }

    Ok(UnlinkReport { removed, pruned })
}

/// Read-only walker that turns the keg tree into a validated op plan.
struct Planner<'a> {
    keg_root: &'a Utf8Path,
    prefix_path: &'a Utf8Path,
    cellar: &'a Utf8Path,
    overwrite: bool,
    ops: Vec<Op>,
    conflicts: Vec<Utf8PathBuf>,
    would_link: Vec<Utf8PathBuf>,
}

impl Planner<'_> {
    /// Walk one top-level link directory `dir` rooted at keg subdir `root`.
    fn walk(&mut self, dir: &str, root: &Utf8Path) -> Result<(), PourError> {
        for src in sorted_children(root)? {
            self.visit(dir, root, &src)?;
        }
        Ok(())
    }

    fn visit(&mut self, dir: &str, root: &Utf8Path, src: &Utf8Path) -> Result<(), PourError> {
        let meta = symlink_meta(src)?;
        let file_type = meta.file_type();
        let is_symlink = file_type.is_symlink();
        let dst = self.dst_for(src);
        let rel = relative_str(root, src);

        if is_symlink || file_type.is_file() {
            // Global prunes (Homebrew: link_dir file branch).
            if base_name(src) == ".DS_Store" {
                return Ok(());
            }
            if is_symlink && symlink_abs_target(src).as_deref() == Some(dst.as_path()) {
                return Ok(()); // self-referential link
            }
            if is_pyc(src) && src.as_str().contains("/site-packages/") {
                return Ok(());
            }

            match strategy_for(dir, &rel) {
                Strategy::SkipFile => Ok(()),
                Strategy::Info if base_name(src) == "dir" => Ok(()),
                // A file yielding :skip_dir or :mkpath still links (else branch).
                _ => self.plan_symlink(&dst, src),
            }
        } else if file_type.is_dir() {
            // A real directory already in place: descend, link nothing here.
            if matches!(self.classify(&dst, src)?, DstState::RealDir) {
                return self.walk_subtree(dir, root, src);
            }
            if src.extension() == Some("app") {
                return Ok(()); // never expose .app bundles
            }

            match strategy_for(dir, &rel) {
                Strategy::SkipDir => Ok(()),
                Strategy::Mkpath => {
                    self.plan_mkpath(&dst, src)?;
                    self.walk_subtree(dir, root, src)
                }
                // :link (and anything not skip/mkpath) symlinks the whole dir,
                // unless an existing cellar symlink must be expanded to merge.
                _ => match self.classify(&dst, src)? {
                    DstState::SymlinkToCellarDir(real) => {
                        self.ops.push(Op::Expand {
                            dst: dst.clone(),
                            real,
                        });
                        self.walk_subtree(dir, root, src)
                    }
                    DstState::BrokenSymlink => self.push_symlink(&dst, src, Pre::RemoveSymlink),
                    DstState::SymlinkToKeg => Ok(()),
                    DstState::Absent => self.push_symlink(&dst, src, Pre::None),
                    DstState::Conflict => self.record_conflict(&dst, src),
                    DstState::RealDir => self.walk_subtree(dir, root, src),
                },
            }
        } else {
            Ok(())
        }
    }

    fn walk_subtree(
        &mut self,
        dir: &str,
        root: &Utf8Path,
        src: &Utf8Path,
    ) -> Result<(), PourError> {
        for child in sorted_children(src)? {
            self.visit(dir, root, &child)?;
        }
        Ok(())
    }

    fn plan_symlink(&mut self, dst: &Utf8Path, src: &Utf8Path) -> Result<(), PourError> {
        match self.classify(dst, src)? {
            DstState::Absent => self.push_symlink(dst, src, Pre::None),
            DstState::SymlinkToKeg => Ok(()), // already linked
            DstState::BrokenSymlink => self.push_symlink(dst, src, Pre::RemoveSymlink),
            DstState::SymlinkToCellarDir(_) | DstState::RealDir | DstState::Conflict => {
                self.record_conflict(dst, src)
            }
        }
    }

    fn plan_mkpath(&mut self, dst: &Utf8Path, src: &Utf8Path) -> Result<(), PourError> {
        match self.classify(dst, src)? {
            DstState::Absent => self.ops.push(Op::Mkdir(dst.to_owned())),
            DstState::RealDir => {}
            DstState::BrokenSymlink => self.ops.push(Op::RemoveSymlinkThenMkdir(dst.to_owned())),
            DstState::SymlinkToKeg => {
                // A symlink to this keg's dir where a real dir is now wanted:
                // expand it in place.
                if let Some(real) = canonical(dst) {
                    self.ops.push(Op::Expand {
                        dst: dst.to_owned(),
                        real,
                    });
                }
            }
            DstState::SymlinkToCellarDir(real) => self.ops.push(Op::Expand {
                dst: dst.to_owned(),
                real,
            }),
            DstState::Conflict => {
                if self.overwrite {
                    self.ops.push(Op::BackupThenMkdir(dst.to_owned()));
                } else {
                    self.conflicts.push(dst.to_owned());
                }
            }
        }
        Ok(())
    }

    fn record_conflict(&mut self, dst: &Utf8Path, src: &Utf8Path) -> Result<(), PourError> {
        if self.overwrite {
            self.push_symlink(dst, src, Pre::Backup)
        } else {
            self.conflicts.push(dst.to_owned());
            Ok(())
        }
    }

    fn push_symlink(&mut self, dst: &Utf8Path, src: &Utf8Path, pre: Pre) -> Result<(), PourError> {
        self.ops.push(Op::Symlink {
            dst: dst.to_owned(),
            target: src.to_owned(),
            pre,
        });
        self.would_link.push(dst.to_owned());
        Ok(())
    }

    fn dst_for(&self, src: &Utf8Path) -> Utf8PathBuf {
        let rel = src
            .strip_prefix(self.keg_root)
            .unwrap_or_else(|_| Utf8Path::new(""));
        self.prefix_path.join(rel)
    }

    fn classify(&self, dst: &Utf8Path, src: &Utf8Path) -> Result<DstState, PourError> {
        let lmeta = match fs::symlink_metadata(dst.as_std_path()) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(DstState::Absent),
            Err(err) => return Err(PourError::io("read", dst, err)),
        };

        if lmeta.file_type().is_symlink() {
            let follows = fs::metadata(dst.as_std_path());
            let Ok(target_meta) = follows else {
                return Ok(DstState::BrokenSymlink);
            };
            if symlink_resolves_to(dst, src) {
                return Ok(DstState::SymlinkToKeg);
            }
            if target_meta.is_dir()
                && let Some(real) = canonical(dst)
                && real.starts_with(self.cellar)
            {
                return Ok(DstState::SymlinkToCellarDir(real));
            }
            Ok(DstState::Conflict)
        } else if lmeta.is_dir() {
            Ok(DstState::RealDir)
        } else {
            Ok(DstState::Conflict)
        }
    }
}

/// Execute one planned op, recording created links and backed-up conflicts.
fn apply_op(
    op: Op,
    prefix_path: &Utf8Path,
    backup_dir: &Utf8Path,
    linked: &mut Vec<Utf8PathBuf>,
    backups: &mut Vec<Utf8PathBuf>,
) -> Result<(), PourError> {
    match op {
        Op::Mkdir(dst) => create_dir_all(&dst),
        Op::RemoveSymlinkThenMkdir(dst) => {
            remove_file(&dst)?;
            create_dir_all(&dst)
        }
        Op::BackupThenMkdir(dst) => {
            backup(&dst, prefix_path, backup_dir)?;
            backups.push(dst.clone());
            create_dir_all(&dst)
        }
        Op::Expand { dst, real } => {
            remove_file(&dst)?;
            expand_dir(&dst, &real)
        }
        Op::Symlink { dst, target, pre } => {
            match pre {
                Pre::None => {}
                Pre::RemoveSymlink => remove_file(&dst)?,
                Pre::Backup => {
                    backup(&dst, prefix_path, backup_dir)?;
                    backups.push(dst.clone());
                }
            }
            if let Some(parent) = dst.parent() {
                create_dir_all(parent)?;
            }
            let rel = relative_target(&dst, &target);
            make_symlink(&rel, &dst)?;
            linked.push(dst);
            Ok(())
        }
    }
}

/// Recursively convert an existing cellar symlink target into real prefix
/// directories, relinking the previous keg's files (Homebrew's
/// `resolve_any_conflicts` for the `:mkpath` and `:link` cases).
fn expand_dir(dst: &Utf8Path, real: &Utf8Path) -> Result<(), PourError> {
    create_dir_all(dst)?;
    for child in sorted_children(real)? {
        let name = base_name(&child);
        let target = dst.join(name);
        let meta = symlink_meta(&child)?;
        if meta.file_type().is_dir() {
            expand_dir(&target, &child)?;
        } else if !path_lexists(&target) {
            let rel = relative_target(&target, &child);
            make_symlink(&rel, &target)?;
        }
    }
    Ok(())
}

/// Recursive unlink walker mirroring Homebrew's keg-tree traversal.
fn walk_unlink(
    dir: &Utf8Path,
    keg_root: &Utf8Path,
    prefix_path: &Utf8Path,
    removed: &mut Vec<Utf8PathBuf>,
    owned_dirs: &mut BTreeSet<Utf8PathBuf>,
) -> Result<(), PourError> {
    for src in sorted_children(dir)? {
        let meta = symlink_meta(&src)?;
        let file_type = meta.file_type();
        let rel = src
            .strip_prefix(keg_root)
            .unwrap_or_else(|_| Utf8Path::new(""));
        let dst = prefix_path.join(rel);

        // A prefix symlink (whole-file or whole-directory link) is removed only
        // when it still resolves into this keg; never descend through it.
        if is_symlink(&dst) {
            if symlink_resolves_to(&dst, &src) {
                remove_file(&dst)?;
                removed.push(dst);
            }
            continue;
        }

        if file_type.is_dir() {
            if is_real_dir(&dst) {
                owned_dirs.insert(dst.clone());
            }
            walk_unlink(&src, keg_root, prefix_path, removed, owned_dirs)?;
        }
    }
    Ok(())
}

/// Rewrite `record` as a relative symlink to `keg_path`, replacing a prior
/// symlink or file (Homebrew's `optlink`/linked-record behavior).
///
/// A real directory at the record path is a typed conflict: never recursively
/// delete it, so caller-owned bytes under `opt/` or `linked/` survive.
fn write_record(record: &Utf8Path, keg_path: &Utf8Path) -> Result<(), PourError> {
    if let Ok(meta) = fs::symlink_metadata(record.as_std_path()) {
        if meta.file_type().is_symlink() || meta.is_file() {
            remove_file(record)?;
        } else if meta.is_dir() {
            return Err(PourError::LinkConflict {
                link_source: keg_path.to_owned(),
                target: record.to_owned(),
                reason: format!(
                    "already exists and is a directory. You may want to remove it:\n  rm -r '{record}'\n"
                ),
            });
        }
    }
    if let Some(parent) = record.parent() {
        create_dir_all(parent)?;
    }
    let rel = relative_target(record, keg_path);
    make_symlink(&rel, record)
}

// --- strategy tables (Appendix E) --------------------------------------------

fn strategy_for(dir: &str, rel: &str) -> Strategy {
    match dir {
        "etc" => Strategy::Mkpath,
        "bin" | "sbin" => Strategy::SkipDir,
        "include" => {
            if postgresql_versioned(rel) {
                Strategy::Mkpath
            } else {
                Strategy::Link
            }
        }
        "share" => share_strategy(rel),
        "lib" => lib_strategy(rel),
        "Frameworks" => {
            if rel.ends_with(".framework") || rel.ends_with(".framework/Versions") {
                Strategy::Mkpath
            } else {
                Strategy::Link
            }
        }
        _ => Strategy::Link,
    }
}

fn share_strategy(rel: &str) -> Strategy {
    if is_infofile(rel) {
        return Strategy::Info;
    }
    if rel == "locale/locale.alias"
        || (rel.starts_with("icons/") && rel.ends_with("/icon-theme.cache"))
    {
        return Strategy::SkipFile;
    }
    if is_localedir(rel)
        || rel.starts_with("icons/")
        || rel.starts_with("zsh")
        || rel.starts_with("fish")
        || rel.starts_with("lua/")
        || rel.starts_with("guile/")
        || postgresql_versioned(rel)
        || rel.starts_with("pypy")
        || is_share_path(rel)
    {
        return Strategy::Mkpath;
    }
    Strategy::Link
}

fn lib_strategy(rel: &str) -> Strategy {
    if rel == "charset.alias" {
        return Strategy::SkipFile;
    }
    let mkpath_exact = matches!(
        rel,
        "cps" | "pkgconfig" | "cmake" | "dtrace" | "ghc" | "php"
    );
    let mkpath_prefix = rel.starts_with("gdk-pixbuf")
        || rel.starts_with("gio")
        || rel.starts_with("lua")
        || rel.starts_with("mecab")
        || rel.starts_with("node")
        || rel.starts_with("ocaml")
        || rel.starts_with("perl5")
        || postgresql_versioned(rel)
        || rel.starts_with("pypy")
        || python_versioned(rel)
        || rel.starts_with('R')
        || rel.starts_with("ruby");
    if mkpath_exact || mkpath_prefix {
        Strategy::Mkpath
    } else {
        Strategy::Link
    }
}

/// Directories under `share` that must always be real (Homebrew `SHARE_PATHS`).
const SHARE_PATHS: [&str; 40] = [
    "aclocal",
    "cps",
    "doc",
    "info",
    "java",
    "locale",
    "man",
    "man/man1",
    "man/man2",
    "man/man3",
    "man/man4",
    "man/man5",
    "man/man6",
    "man/man7",
    "man/man8",
    "man/cat1",
    "man/cat2",
    "man/cat3",
    "man/cat4",
    "man/cat5",
    "man/cat6",
    "man/cat7",
    "man/cat8",
    "applications",
    "gnome",
    "gnome/help",
    "icons",
    "mime",
    "mime/packages",
    "mime-info",
    "pixmaps",
    "postgresql",
    "sounds",
    "guile",
    "lua",
    "fish",
    "zsh",
    "pypy",
    "emacs",
    "vim",
];

fn is_share_path(rel: &str) -> bool {
    SHARE_PATHS.contains(&rel)
}

/// `INFOFILE_RX = info/([^.].*?\.info(\.gz)?|dir)$`.
///
/// Matches `info/dir` (via the `|dir` alternative) as well as `.info[.gz]`
/// files; the caller then skips the historical `dir` file by basename.
fn is_infofile(rel: &str) -> bool {
    let Some(rest) = rel.strip_prefix("info/") else {
        return false;
    };
    if rest == "dir" {
        return true; // `|dir` branch: Strategy::Info, skipped by basename guard
    }
    // First char after `info/` must be non-dot; whole path ends in `.info[.gz]`.
    !rest.starts_with('.') && (rel.ends_with(".info") || rel.ends_with(".info.gz"))
}

/// `LOCALEDIR_RX = (locale|man)/([a-z]{2}|C|POSIX)...` (unanchored).
fn is_localedir(rel: &str) -> bool {
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        if (part == "locale" || part == "man")
            && let Some(next) = parts.peek()
            && locale_code_prefix(next)
        {
            return true;
        }
    }
    false
}

fn locale_code_prefix(seg: &str) -> bool {
    if seg.starts_with('C') || seg.starts_with("POSIX") {
        return true;
    }
    let mut chars = seg.chars();
    matches!((chars.next(), chars.next()), (Some(a), Some(b)) if a.is_ascii_lowercase() && b.is_ascii_lowercase())
}

/// `^postgresql@\d+`.
fn postgresql_versioned(rel: &str) -> bool {
    rel.strip_prefix("postgresql@")
        .is_some_and(|rest| rest.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

/// `^python[23]\.\d+`.
fn python_versioned(rel: &str) -> bool {
    let Some(rest) = rel.strip_prefix("python") else {
        return false;
    };
    let mut chars = rest.chars();
    matches!(chars.next(), Some('2' | '3'))
        && chars.next() == Some('.')
        && chars.next().is_some_and(|c| c.is_ascii_digit())
}

fn is_pyc(path: &Utf8Path) -> bool {
    matches!(path.extension(), Some("pyc" | "pyo"))
}

// --- filesystem helpers ------------------------------------------------------

fn sorted_children(dir: &Utf8Path) -> Result<Vec<Utf8PathBuf>, PourError> {
    let iter = fs::read_dir(dir.as_std_path()).map_err(|err| PourError::io("read", dir, err))?;
    let mut paths = Vec::new();
    for entry in iter {
        let entry = entry.map_err(|err| PourError::io("read", dir, err))?;
        let path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| {
            PourError::io(
                "read",
                dir,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("non-utf8 path {}", path.display()),
                ),
            )
        })?;
        paths.push(path);
    }
    paths.sort();
    Ok(paths)
}

fn symlink_meta(path: &Utf8Path) -> Result<fs::Metadata, PourError> {
    fs::symlink_metadata(path.as_std_path()).map_err(|err| PourError::io("read", path, err))
}

fn create_dir_all(dir: &Utf8Path) -> Result<(), PourError> {
    fs::create_dir_all(dir.as_std_path()).map_err(|err| PourError::io("create", dir, err))
}

fn remove_file(path: &Utf8Path) -> Result<(), PourError> {
    fs::remove_file(path.as_std_path()).map_err(|err| PourError::io("remove", path, err))
}

/// Collect destinations that overwrite would back up, without mutating.
fn planned_backups(ops: &[Op]) -> Vec<Utf8PathBuf> {
    let mut backups = Vec::new();
    for op in ops {
        match op {
            Op::BackupThenMkdir(dst) => backups.push(dst.clone()),
            Op::Symlink {
                dst,
                pre: Pre::Backup,
                ..
            } => backups.push(dst.clone()),
            _ => {}
        }
    }
    backups
}

/// Move `dst` (a conflict) under `backup_dir`, preserving its prefix-relative
/// path.
///
/// Construction requires `dst.strip_prefix(prefix)` (no absolute-join fallback),
/// verifies the Backup root and every ancestor are real directories (never
/// symlinks), creates missing parents one segment at a time, and refuses a
/// planted symlink at any Backup path component before `rename`.
fn backup(dst: &Utf8Path, prefix_path: &Utf8Path, backup_dir: &Utf8Path) -> Result<(), PourError> {
    let rel = dst
        .strip_prefix(prefix_path)
        .map_err(|_| PourError::LinkConflict {
            link_source: dst.to_owned(),
            target: backup_dir.to_owned(),
            reason: format!("is not under the prefix '{prefix_path}'"),
        })?;

    for component in rel.components() {
        if !matches!(component, camino::Utf8Component::Normal(_)) {
            return Err(PourError::LinkConflict {
                link_source: dst.to_owned(),
                target: backup_dir.join(rel),
                reason: format!("backup relative path '{rel}' contains non-normal components"),
            });
        }
    }

    ensure_real_backup_root(backup_dir)?;
    let target = create_backup_parents(backup_dir, rel)?;

    if is_symlink(&target) {
        return Err(PourError::LinkConflict {
            link_source: dst.to_owned(),
            target: target.clone(),
            reason: "refusing to rename onto a symlink under Backup".to_owned(),
        });
    }

    fs::rename(dst.as_std_path(), target.as_std_path())
        .map_err(|err| PourError::io("backup", dst, err))
}

/// Ensure `backup_dir` exists as a real directory; refuse a planted symlink.
fn ensure_real_backup_root(backup_dir: &Utf8Path) -> Result<(), PourError> {
    match fs::symlink_metadata(backup_dir.as_std_path()) {
        Ok(meta) if meta.file_type().is_symlink() => Err(PourError::LinkConflict {
            link_source: backup_dir.to_owned(),
            target: backup_dir.to_owned(),
            reason: "Backup root is a symlink; refusing to follow it".to_owned(),
        }),
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(PourError::LinkConflict {
            link_source: backup_dir.to_owned(),
            target: backup_dir.to_owned(),
            reason: "Backup root exists and is not a directory".to_owned(),
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = backup_dir.parent() {
                ensure_real_dir_or_create_all(parent)?;
            }
            fs::create_dir(backup_dir.as_std_path())
                .map_err(|err| PourError::io("create", backup_dir, err))
        }
        Err(err) => Err(PourError::io("read", backup_dir, err)),
    }
}

/// Create missing parents of `backup_dir/rel` one segment at a time, refusing
/// any symlink along the way. Returns the final backup target path.
fn create_backup_parents(backup_dir: &Utf8Path, rel: &Utf8Path) -> Result<Utf8PathBuf, PourError> {
    let mut cur = backup_dir.to_owned();
    let components: Vec<_> = rel.components().collect();
    if components.is_empty() {
        return Err(PourError::LinkConflict {
            link_source: backup_dir.to_owned(),
            target: backup_dir.to_owned(),
            reason: "refusing to back up the prefix root into Backup".to_owned(),
        });
    }

    for (index, component) in components.iter().enumerate() {
        cur = cur.join(component.as_str());
        let is_final = index + 1 == components.len();
        match fs::symlink_metadata(cur.as_std_path()) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(PourError::LinkConflict {
                    link_source: backup_dir.to_owned(),
                    target: cur,
                    reason: "refusing planted symlink under Backup before rename".to_owned(),
                });
            }
            Ok(meta) if meta.is_dir() => {
                if is_final {
                    return Err(PourError::LinkConflict {
                        link_source: backup_dir.to_owned(),
                        target: cur,
                        reason: "backup target already exists as a directory".to_owned(),
                    });
                }
            }
            Ok(_) => {
                if !is_final {
                    return Err(PourError::LinkConflict {
                        link_source: backup_dir.to_owned(),
                        target: cur,
                        reason: "backup path ancestor exists and is not a directory".to_owned(),
                    });
                }
                // Final leaf exists as a non-dir: rename will replace it.
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                if !is_final {
                    fs::create_dir(cur.as_std_path())
                        .map_err(|err| PourError::io("create", &cur, err))?;
                }
            }
            Err(err) => return Err(PourError::io("read", &cur, err)),
        }
    }
    Ok(cur)
}

/// Ensure `dir` is a real directory, creating missing segments without
/// following a symlink that already occupies a path component.
fn ensure_real_dir_or_create_all(dir: &Utf8Path) -> Result<(), PourError> {
    match fs::symlink_metadata(dir.as_std_path()) {
        Ok(meta) if meta.file_type().is_symlink() => Err(PourError::LinkConflict {
            link_source: dir.to_owned(),
            target: dir.to_owned(),
            reason: "refusing to create Backup under a symlink parent".to_owned(),
        }),
        Ok(meta) if meta.is_dir() => Ok(()),
        Ok(_) => Err(PourError::LinkConflict {
            link_source: dir.to_owned(),
            target: dir.to_owned(),
            reason: "Backup parent exists and is not a directory".to_owned(),
        }),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent()
                && parent != dir
            {
                ensure_real_dir_or_create_all(parent)?;
            }
            match fs::create_dir(dir.as_std_path()) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    if is_real_dir(dir) {
                        Ok(())
                    } else {
                        Err(PourError::LinkConflict {
                            link_source: dir.to_owned(),
                            target: dir.to_owned(),
                            reason: "Backup parent path is not a real directory".to_owned(),
                        })
                    }
                }
                Err(err) => Err(PourError::io("create", dir, err)),
            }
        }
        Err(err) => Err(PourError::io("read", dir, err)),
    }
}

/// Remove `.DS_Store` if present, then rmdir; report whether the dir is gone.
fn rmdir_if_possible(dir: &Utf8Path) -> Result<bool, PourError> {
    let ds_store = dir.join(".DS_Store");
    if path_lexists(&ds_store) {
        let _ = fs::remove_file(ds_store.as_std_path());
    }
    match fs::remove_dir(dir.as_std_path()) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(_) => Ok(false), // non-empty or not permitted: leave it be
    }
}

fn must_exist_set(prefix: &Prefix) -> BTreeSet<Utf8PathBuf> {
    let mut set: BTreeSet<Utf8PathBuf> = MUST_EXIST_TOP
        .iter()
        .map(|dir| prefix.path().join(dir))
        .collect();
    set.insert(prefix.linked().to_owned());
    set
}

fn path_exists(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path.as_std_path())
        .map(|meta| meta.file_type().is_symlink() || meta.is_dir() || meta.is_file())
        .unwrap_or(false)
}

/// True when the path itself exists (following no final symlink resolution).
fn path_lexists(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path.as_std_path()).is_ok()
}

fn is_symlink(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path.as_std_path())
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

fn is_real_dir(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path.as_std_path())
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

/// Canonical (fully resolved) path, or `None` when it cannot be resolved.
fn canonical(path: &Utf8Path) -> Option<Utf8PathBuf> {
    let real = fs::canonicalize(path.as_std_path()).ok()?;
    Utf8PathBuf::from_path_buf(real).ok()
}

/// True when `link` fully resolves to the same real path as `expected`.
fn symlink_resolves_to(link: &Utf8Path, expected: &Utf8Path) -> bool {
    match (canonical(link), canonical(expected)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// Absolute target of a symlink without resolving further links.
fn symlink_abs_target(link: &Utf8Path) -> Option<Utf8PathBuf> {
    let target = fs::read_link(link.as_std_path()).ok()?;
    let target = Utf8PathBuf::from_path_buf(target).ok()?;
    if target.is_absolute() {
        Some(target)
    } else {
        Some(link.parent()?.join(target))
    }
}

fn base_name(path: &Utf8Path) -> &str {
    path.file_name().unwrap_or(path.as_str())
}

fn relative_str(root: &Utf8Path, src: &Utf8Path) -> String {
    src.strip_prefix(root)
        .map(|rel| rel.as_str().to_owned())
        .unwrap_or_default()
}

/// Relative symlink body pointing from `dst` to absolute `target`.
fn relative_target(dst: &Utf8Path, target: &Utf8Path) -> Utf8PathBuf {
    let base = dst.parent().unwrap_or(Utf8Path::new(""));
    let base_parts: Vec<&str> = base.components().map(|c| c.as_str()).collect();
    let target_parts: Vec<&str> = target.components().map(|c| c.as_str()).collect();

    let mut shared = 0;
    while shared < base_parts.len()
        && shared < target_parts.len()
        && base_parts[shared] == target_parts[shared]
    {
        shared += 1;
    }

    let mut rel = Utf8PathBuf::new();
    for _ in shared..base_parts.len() {
        rel.push("..");
    }
    for part in &target_parts[shared..] {
        rel.push(part);
    }
    rel
}

#[cfg(unix)]
fn make_symlink(target: &Utf8Path, link: &Utf8Path) -> Result<(), PourError> {
    std::os::unix::fs::symlink(target.as_std_path(), link.as_std_path())
        .map_err(|err| PourError::io("symlink", link, err))
}

#[cfg(not(unix))]
fn make_symlink(target: &Utf8Path, link: &Utf8Path) -> Result<(), PourError> {
    let _ = target;
    Err(PourError::io(
        "symlink",
        link,
        io::Error::new(
            io::ErrorKind::Unsupported,
            "keg linking requires a Unix target",
        ),
    ))
}
