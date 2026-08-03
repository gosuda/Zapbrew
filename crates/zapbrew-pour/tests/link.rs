//! Temp-prefix integration tests for keg link/unlink.
//!
//! Every case builds a real keg tree under a scratch cellar, links it into a
//! scratch prefix, and inspects the resulting symlink set. Snapshots capture
//! the per-directory strategy table (Appendix E); the remaining cases assert
//! conflict, overwrite, dry-run backups, force, keg-only, relative-record,
//! record-directory conflict, backup symlink-escape, and ownership-safe
//! unlink behavior directly.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::TempDir;
use zapbrew_pour::{LinkOptions, PourError, link, unlink};
use zapbrew_prefix::{Env, EnvDetectInput, Keg, Prefix, SystemCommandRunner};
use zapbrew_types::{FormulaName, PkgVersion};

struct Fixture {
    _tmp: TempDir,
    prefix: Prefix,
    keg: Keg,
}

impl Fixture {
    fn prefix_path(&self) -> &Utf8Path {
        self.prefix.path()
    }

    fn keg_path(&self) -> Utf8PathBuf {
        self.keg.path().to_owned()
    }
}

fn utf8(path: std::path::PathBuf) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(path).expect("temp path is utf-8")
}

/// Build a scratch prefix + empty keg for formula `foo` version `1.0`.
fn fixture() -> Fixture {
    let tmp = TempDir::new().expect("tempdir");
    let root = utf8(tmp.path().to_path_buf());
    let prefix_path = root.join("prefix");
    let cellar = prefix_path.join("Cellar");
    let cache = root.join("cache");

    fs::create_dir_all(prefix_path.as_std_path()).expect("mkdir prefix");
    fs::create_dir_all(cellar.as_std_path()).expect("mkdir cellar");

    let mut vars = HashMap::new();
    vars.insert("HOMEBREW_PREFIX".to_owned(), prefix_path.to_string());
    vars.insert("HOMEBREW_CELLAR".to_owned(), cellar.to_string());
    vars.insert("HOMEBREW_CACHE".to_owned(), cache.to_string());

    let input = EnvDetectInput {
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        home: root,
        xdg_cache_home: None,
        vars,
        available_parallelism: 2,
    };
    let env = Env::detect_from(&input, &SystemCommandRunner).expect("detect env");
    let prefix = Prefix::new(env);

    let name = FormulaName::from_str("foo").expect("name");
    let version = PkgVersion::from_str("1.0").expect("version");
    let keg = Keg::new(prefix.cellar(), name, version).expect("keg");
    fs::create_dir_all(keg.path().as_std_path()).expect("mkdir keg");

    Fixture {
        _tmp: tmp,
        prefix,
        keg,
    }
}

fn touch(path: &Utf8Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent.as_std_path()).expect("mkdir parent");
    }
    fs::write(path.as_std_path(), contents).expect("write file");
}

fn keg_file(fx: &Fixture, rel: &str, contents: &str) {
    touch(&fx.keg_path().join(rel), contents);
}

fn is_symlink(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path.as_std_path())
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

fn lexists(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path.as_std_path()).is_ok()
}

fn read_link(path: &Utf8Path) -> String {
    fs::read_link(path.as_std_path())
        .expect("read_link")
        .to_string_lossy()
        .into_owned()
}

/// Collect every symlink under `root` (never following one), returning
/// `"relpath => target"` lines sorted, excluding the opt and linked records.
fn symlink_lines(root: &Utf8Path) -> Vec<String> {
    let mut out = Vec::new();
    collect(root, root, &mut out);
    out.retain(|line| !line.starts_with("opt/") && !line.starts_with("var/"));
    out.sort();
    out
}

fn collect(root: &Utf8Path, dir: &Utf8Path, out: &mut Vec<String>) {
    let entries = match fs::read_dir(dir.as_std_path()) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = utf8(entry.path());
        let meta = fs::symlink_metadata(path.as_std_path()).expect("lstat");
        let rel = path.strip_prefix(root).unwrap_or(&path);
        if meta.file_type().is_symlink() {
            out.push(format!("{rel} => {}", read_link(&path)));
        } else if meta.is_dir() {
            collect(root, &path, out);
        }
    }
}

// --- per-directory strategy table --------------------------------------------

#[test]
fn links_every_directory_per_appendix_e_with_relative_targets() {
    let fx = fixture();

    // bin/sbin: files inside are linked individually; subdirectories skipped.
    keg_file(&fx, "bin/hello", "");
    keg_file(&fx, "bin/tools/helper", ""); // bin subdir -> skip_dir, not linked
    keg_file(&fx, "sbin/daemon", "");
    // etc: real directories, files linked.
    keg_file(&fx, "etc/foo.conf", "");
    keg_file(&fx, "etc/sub/nested.conf", "");
    // include: whole-dir link, postgresql@N mkpath, plain file link.
    keg_file(&fx, "include/foo.h", "");
    keg_file(&fx, "include/mylib/header.h", ""); // dir -> :link (whole dir)
    keg_file(&fx, "include/postgresql@14/pg.h", ""); // dir -> :mkpath
    // lib: mkpath sets, charset.alias skipped, plain file link.
    keg_file(&fx, "lib/libfoo.dylib", "");
    keg_file(&fx, "lib/pkgconfig/foo.pc", "");
    keg_file(&fx, "lib/charset.alias", ""); // :skip_file
    keg_file(&fx, "lib/python3.11/site.py", "");
    keg_file(&fx, "lib/python3.11/site-packages/mod.pyc", ""); // pyc -> pruned
    // share: man mkpath, info link, historical `dir` skip, locale.alias skip,
    // doc mkpath, unknown subdir whole-dir link.
    keg_file(&fx, "share/man/man1/foo.1", "");
    keg_file(&fx, "share/info/foo.info", "");
    keg_file(&fx, "share/info/dir", ""); // :info + basename dir -> skip
    keg_file(&fx, "share/locale/locale.alias", ""); // :skip_file
    keg_file(&fx, "share/doc/readme", "");
    keg_file(&fx, "share/misc/data", ""); // unknown subdir -> :link (whole dir)
    touch(&fx.keg_path().join("share/.DS_Store"), ""); // pruned
    // var is never walked by link.
    keg_file(&fx, "var/foo/state", "");

    let report = link(&fx.keg, &fx.prefix, LinkOptions::default()).expect("link");
    assert!(report.conflicts.is_empty(), "no conflicts expected");
    assert!(report.backups.is_empty(), "no backups expected");

    let rendered = symlink_lines(fx.prefix_path()).join("\n");
    insta::assert_snapshot!(rendered, @r"
    bin/hello => ../Cellar/foo/1.0/bin/hello
    etc/foo.conf => ../Cellar/foo/1.0/etc/foo.conf
    etc/sub/nested.conf => ../../Cellar/foo/1.0/etc/sub/nested.conf
    include/foo.h => ../Cellar/foo/1.0/include/foo.h
    include/mylib => ../Cellar/foo/1.0/include/mylib
    include/postgresql@14/pg.h => ../../Cellar/foo/1.0/include/postgresql@14/pg.h
    lib/libfoo.dylib => ../Cellar/foo/1.0/lib/libfoo.dylib
    lib/pkgconfig/foo.pc => ../../Cellar/foo/1.0/lib/pkgconfig/foo.pc
    lib/python3.11/site.py => ../../Cellar/foo/1.0/lib/python3.11/site.py
    sbin/daemon => ../Cellar/foo/1.0/sbin/daemon
    share/doc/readme => ../../Cellar/foo/1.0/share/doc/readme
    share/info/foo.info => ../../Cellar/foo/1.0/share/info/foo.info
    share/man/man1/foo.1 => ../../../Cellar/foo/1.0/share/man/man1/foo.1
    share/misc => ../Cellar/foo/1.0/share/misc
    ");

    // Compiled skip rules leave nothing behind.
    let p = fx.prefix_path();
    assert!(!lexists(&p.join("bin/tools")), "bin subdir skipped");
    assert!(
        !lexists(&p.join("lib/charset.alias")),
        "charset.alias skipped"
    );
    assert!(!lexists(&p.join("share/info/dir")), "info dir skipped");
    assert!(
        !lexists(&p.join("share/locale/locale.alias")),
        "locale.alias skipped"
    );
    assert!(!lexists(&p.join("share/.DS_Store")), ".DS_Store skipped");
    assert!(
        !lexists(&p.join("lib/python3.11/site-packages/mod.pyc")),
        "site-packages pyc skipped"
    );
    assert!(
        !lexists(&p.join("var/foo")),
        "var keg contents never linked"
    );

    // Whole-dir links descend to nothing; their contents are reached through
    // the single directory symlink.
    assert!(
        is_symlink(&p.join("include/mylib")),
        "mylib is a dir symlink"
    );
    assert!(is_symlink(&p.join("share/misc")), "misc is a dir symlink");

    // mkpath directories are real, not symlinks.
    assert!(
        fs::symlink_metadata(p.join("include/postgresql@14").as_std_path())
            .expect("lstat postgresql dir")
            .is_dir()
    );

    // opt and linked records are relative symlinks to the keg.
    assert_eq!(read_link(&p.join("opt/foo")), "../Cellar/foo/1.0");
    assert_eq!(
        read_link(&p.join("var/homebrew/linked/foo")),
        "../../../Cellar/foo/1.0"
    );
}

// --- dry run ------------------------------------------------------------------

#[test]
fn dry_run_reports_plan_and_writes_nothing() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "");
    keg_file(&fx, "sbin/daemon", "");

    let options = LinkOptions {
        dry_run: true,
        ..LinkOptions::default()
    };
    let report = link(&fx.keg, &fx.prefix, options).expect("link");

    let p = fx.prefix_path();
    assert_eq!(
        report.linked,
        vec![p.join("bin/hello"), p.join("sbin/daemon")]
    );
    assert!(!lexists(&p.join("bin/hello")), "no file link written");
    assert!(!lexists(&p.join("sbin/daemon")), "no file link written");
    assert!(!lexists(&p.join("opt/foo")), "no opt record written");
    assert!(
        !lexists(&p.join("var/homebrew/linked/foo")),
        "no linked record written"
    );
}

// --- conflict / overwrite -----------------------------------------------------

#[test]
fn conflict_without_overwrite_aborts_before_any_mutation() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "keg");
    keg_file(&fx, "sbin/daemon", "keg");

    let p = fx.prefix_path();
    touch(&p.join("bin/hello"), "preexisting"); // real file blocks the link

    let report = link(&fx.keg, &fx.prefix, LinkOptions::default()).expect("link");

    assert_eq!(report.conflicts, vec![p.join("bin/hello")]);
    assert!(report.linked.is_empty(), "aborted: nothing linked");
    assert!(report.backups.is_empty());
    // Preflight abort: the unrelated file link and records were never created.
    assert!(!lexists(&p.join("sbin/daemon")), "no partial mutation");
    assert!(!lexists(&p.join("opt/foo")), "no opt record on abort");
    assert!(!is_symlink(&p.join("bin/hello")), "original file untouched");
    assert_eq!(
        fs::read_to_string(p.join("bin/hello").as_std_path()).expect("read hello"),
        "preexisting"
    );
}

#[test]
fn overwrite_backs_up_conflicts_preserving_relative_path() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "keg");

    let p = fx.prefix_path();
    touch(&p.join("bin/hello"), "old");

    let options = LinkOptions {
        overwrite: true,
        ..LinkOptions::default()
    };
    let report = link(&fx.keg, &fx.prefix, options).expect("link");

    assert!(report.conflicts.is_empty(), "overwrite resolves conflicts");
    assert_eq!(report.backups, vec![p.join("bin/hello")]);
    assert!(is_symlink(&p.join("bin/hello")), "now a keg symlink");
    assert_eq!(
        fs::read_to_string(p.join("bin/hello").as_std_path()).expect("read hello"),
        "keg",
        "symlink resolves to keg content"
    );

    let backup = fx._tmp.path().join("cache/Backup/bin/hello");
    assert_eq!(
        fs::read_to_string(&backup).expect("read backup"),
        "old",
        "conflict preserved in Backup"
    );
}

#[test]
fn broken_symlink_at_target_is_replaced_without_conflict() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "keg");

    let p = fx.prefix_path();
    fs::create_dir_all(p.join("bin").as_std_path()).expect("mkdir bin");
    symlink("nowhere", p.join("bin/hello").as_std_path()).expect("make broken symlink");

    let report = link(&fx.keg, &fx.prefix, LinkOptions::default()).expect("link");

    assert!(
        report.conflicts.is_empty(),
        "broken symlink is not a conflict"
    );
    assert_eq!(report.linked, vec![p.join("bin/hello")]);
    assert_eq!(
        fs::read_to_string(p.join("bin/hello").as_std_path()).expect("read hello"),
        "keg"
    );
}

#[test]
fn self_referential_keg_symlink_is_skipped() {
    let fx = fixture();
    let p = fx.prefix_path();
    // A keg symlink pointing back at its own prefix destination.
    fs::create_dir_all(fx.keg_path().join("bin").as_std_path()).expect("mkdir keg bin");
    symlink(
        p.join("bin/loop").as_std_path(),
        fx.keg_path().join("bin/loop").as_std_path(),
    )
    .expect("make self-ref symlink");

    let report = link(&fx.keg, &fx.prefix, LinkOptions::default()).expect("link");

    assert!(report.linked.is_empty(), "self-referential link skipped");
    assert!(!lexists(&p.join("bin/loop")), "nothing written");
}

// --- keg-only / force ---------------------------------------------------------

#[test]
fn keg_only_skips_file_links_but_writes_records() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "");

    let options = LinkOptions {
        keg_only: true,
        ..LinkOptions::default()
    };
    let report = link(&fx.keg, &fx.prefix, options).expect("link");

    let p = fx.prefix_path();
    assert!(
        report.linked.is_empty(),
        "no prefix file links for keg-only"
    );
    assert!(!lexists(&p.join("bin/hello")), "no bin symlink");
    assert!(is_symlink(&p.join("opt/foo")), "opt record still written");
    assert!(
        is_symlink(&p.join("var/homebrew/linked/foo")),
        "linked record still written"
    );
}

#[test]
fn force_links_keg_only_into_prefix() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "");

    let options = LinkOptions {
        keg_only: true,
        force: true,
        ..LinkOptions::default()
    };
    let report = link(&fx.keg, &fx.prefix, options).expect("link");

    let p = fx.prefix_path();
    assert_eq!(report.linked, vec![p.join("bin/hello")]);
    assert!(is_symlink(&p.join("bin/hello")), "forced link created");
    assert!(is_symlink(&p.join("opt/foo")));
}

// --- unlink -------------------------------------------------------------------

#[test]
fn unlink_removes_keg_links_prunes_owned_dirs_and_keeps_opt() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "");
    keg_file(&fx, "etc/sub/nested.conf", "");
    keg_file(&fx, "include/mylib/header.h", ""); // whole-dir symlink
    keg_file(&fx, "lib/pkgconfig/foo.pc", "");
    keg_file(&fx, "share/man/man1/foo.1", "");

    link(&fx.keg, &fx.prefix, LinkOptions::default()).expect("link");
    let p = fx.prefix_path();
    assert!(is_symlink(&p.join("include/mylib")));

    let report = unlink(&fx.keg, &fx.prefix).expect("unlink");

    // Every keg symlink is gone (including the whole-dir link).
    assert!(symlink_lines(p).is_empty(), "all keg symlinks removed");
    assert!(!lexists(&p.join("include/mylib")));

    // Emptied owned directories are pruned deepest-first.
    for pruned in ["etc/sub", "lib/pkgconfig", "share/man/man1", "share/man"] {
        assert!(!lexists(&p.join(pruned)), "{pruned} pruned");
        assert!(
            report.pruned.contains(&p.join(pruned)),
            "{pruned} reported pruned"
        );
    }

    // must-exist directories survive.
    for keep in ["bin", "etc", "include", "lib", "sbin", "share", "opt"] {
        assert!(lexists(&p.join(keep)), "{keep} retained");
    }

    // opt retained, linked record dropped.
    assert!(
        is_symlink(&p.join("opt/foo")),
        "opt retained until uninstall"
    );
    assert!(
        !lexists(&p.join("var/homebrew/linked/foo")),
        "linked record removed"
    );
}

#[test]
fn unlink_only_touches_symlinks_that_resolve_into_the_keg() {
    let fx = fixture();
    keg_file(&fx, "bin/a", "");
    keg_file(&fx, "bin/b", "");
    keg_file(&fx, "bin/c", "");
    keg_file(&fx, "bin/d", "");

    link(&fx.keg, &fx.prefix, LinkOptions::default()).expect("link");
    let p = fx.prefix_path();

    // b: replace our link with a foreign symlink pointing outside the keg.
    fs::remove_file(p.join("bin/b").as_std_path()).expect("rm b");
    symlink("/bin/sh", p.join("bin/b").as_std_path()).expect("foreign symlink");
    // c: replace our link with a real file.
    fs::remove_file(p.join("bin/c").as_std_path()).expect("rm c");
    touch(&p.join("bin/c"), "real");
    // d: delete the keg target so the prefix link dangles into the keg.
    fs::remove_file(fx.keg_path().join("bin/d").as_std_path()).expect("rm keg d");

    let report = unlink(&fx.keg, &fx.prefix).expect("unlink");

    assert_eq!(
        report.removed,
        vec![p.join("bin/a")],
        "only our live link removed"
    );
    assert!(!lexists(&p.join("bin/a")), "a removed");
    assert!(is_symlink(&p.join("bin/b")), "foreign symlink preserved");
    assert_eq!(
        fs::read_to_string(p.join("bin/c").as_std_path()).expect("read c"),
        "real",
        "real file preserved"
    );
    assert!(is_symlink(&p.join("bin/d")), "dangling link preserved");
}

// --- dry-run overwrite backups ------------------------------------------------

#[test]
fn dry_run_overwrite_reports_would_be_backups_from_backup_ops() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "keg");
    // Empty mkpath dir -> BackupThenMkdir when the prefix path is a real file.
    fs::create_dir_all(fx.keg_path().join("lib/pkgconfig").as_std_path())
        .expect("mkdir keg lib/pkgconfig");

    let p = fx.prefix_path();
    touch(&p.join("bin/hello"), "old"); // Symlink Pre::Backup
    touch(&p.join("lib/pkgconfig"), "not-a-dir"); // BackupThenMkdir

    let options = LinkOptions {
        dry_run: true,
        overwrite: true,
        ..LinkOptions::default()
    };
    let report = link(&fx.keg, &fx.prefix, options).expect("dry-run link");

    assert!(
        report.conflicts.is_empty(),
        "overwrite plans backups, not conflicts"
    );
    assert!(
        report.backups.contains(&p.join("bin/hello")),
        "Symlink Pre::Backup destination reported: {:?}",
        report.backups
    );
    assert!(
        report.backups.contains(&p.join("lib/pkgconfig")),
        "BackupThenMkdir destination reported: {:?}",
        report.backups
    );

    // Mutate nothing: originals intact, records absent, Backup never created.
    assert!(!is_symlink(&p.join("bin/hello")));
    assert_eq!(
        fs::read_to_string(p.join("bin/hello").as_std_path()).expect("read hello"),
        "old"
    );
    assert_eq!(
        fs::read_to_string(p.join("lib/pkgconfig").as_std_path()).expect("read pkgconfig"),
        "not-a-dir"
    );
    assert!(!lexists(&p.join("opt/foo")), "no opt record on dry-run");
    assert!(
        !fx._tmp.path().join("cache/Backup").exists(),
        "dry-run must not create Backup"
    );
}

// --- write_record directory conflict ------------------------------------------

#[test]
fn write_record_refuses_real_directory_preserving_bytes() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "keg");

    let p = fx.prefix_path();
    let opt = p.join("opt/foo");
    fs::create_dir_all(opt.as_std_path()).expect("mkdir opt/foo");
    let marker = opt.join("precious.txt");
    fs::write(marker.as_std_path(), "do-not-delete").expect("write marker");

    let err = match link(&fx.keg, &fx.prefix, LinkOptions::default()) {
        Err(err) => err,
        Ok(report) => panic!("expected LinkConflict, got success {report:?}"),
    };
    match err {
        PourError::LinkConflict { target, reason, .. } => {
            assert_eq!(target, opt);
            assert!(
                reason.contains("already exists and is a directory"),
                "reason={reason}"
            );
        }
        other => panic!("expected LinkConflict, got {other}"),
    }

    assert!(
        opt.as_std_path().is_dir() && !is_symlink(&opt),
        "opt/foo must remain a real directory"
    );
    assert_eq!(
        fs::read_to_string(marker.as_std_path()).expect("read marker"),
        "do-not-delete",
        "bytes under the real directory must be preserved"
    );
}

// --- backup symlink escape ----------------------------------------------------

#[test]
fn planted_backup_bin_symlink_refused_and_outside_unchanged() {
    let fx = fixture();
    keg_file(&fx, "bin/hello", "keg");

    let p = fx.prefix_path();
    touch(&p.join("bin/hello"), "old");

    let outside = utf8(fx._tmp.path().join("outside-target"));
    fs::create_dir_all(outside.as_std_path()).expect("mkdir outside");
    let marker = outside.join("KEEPME");
    fs::write(marker.as_std_path(), "precious").expect("write outside marker");

    let backup_root = utf8(fx._tmp.path().join("cache/Backup"));
    fs::create_dir_all(backup_root.as_std_path()).expect("mkdir Backup");
    // Plant Backup/bin -> outside so a naive create_dir_all/rename would escape.
    symlink(outside.as_std_path(), backup_root.join("bin").as_std_path())
        .expect("plant Backup/bin symlink");

    let options = LinkOptions {
        overwrite: true,
        ..LinkOptions::default()
    };
    let err = match link(&fx.keg, &fx.prefix, options) {
        Err(err) => err,
        Ok(report) => panic!("expected refusal of planted Backup symlink, got {report:?}"),
    };
    match err {
        PourError::LinkConflict { reason, .. } => {
            assert!(
                reason.contains("planted symlink") || reason.contains("symlink"),
                "reason={reason}"
            );
        }
        other => panic!("expected LinkConflict, got {other}"),
    }

    assert_eq!(
        fs::read_to_string(marker.as_std_path()).expect("read outside marker"),
        "precious",
        "outside target must remain unchanged"
    );
    assert!(
        !outside.join("hello").as_std_path().exists(),
        "conflict must not be renamed into the outside target"
    );
    assert_eq!(
        fs::read_to_string(p.join("bin/hello").as_std_path()).expect("read hello"),
        "old",
        "prefix conflict must remain until a safe backup succeeds"
    );
    assert!(
        !is_symlink(&p.join("bin/hello")),
        "no link written after refusal"
    );
}

// --- prefix destination ancestor symlink confinement --------------------------

#[test]
fn prefix_bin_symlink_blocks_link_and_outside_unchanged() {
    let fx = fixture();
    keg_file(&fx, "bin/tool", "keg-tool");

    let p = fx.prefix_path();
    // Remove the real bin created by the fixture's prefix layout if present,
    // then plant prefix/bin -> outside so linking bin/tool would escape.
    let bin = p.join("bin");
    if bin.as_std_path().exists() {
        fs::remove_dir_all(bin.as_std_path()).expect("remove real bin");
    }
    let outside = utf8(fx._tmp.path().join("outside-prefix-bin"));
    fs::create_dir_all(outside.as_std_path()).expect("mkdir outside");
    let marker = outside.join("KEEPME");
    fs::write(marker.as_std_path(), "precious-outside").expect("write outside marker");
    symlink(outside.as_std_path(), bin.as_std_path()).expect("plant prefix/bin symlink");

    let err = match link(&fx.keg, &fx.prefix, LinkOptions::default()) {
        Err(err) => err,
        Ok(report) => panic!("expected LinkConflict for prefix/bin symlink, got {report:?}"),
    };
    match err {
        PourError::LinkConflict { reason, target, .. } => {
            assert!(
                reason.contains("destination ancestor is a symlink") || reason.contains("symlink"),
                "reason={reason}"
            );
            assert_eq!(target, bin);
        }
        other => panic!("expected LinkConflict, got {other}"),
    }

    assert_eq!(
        fs::read_to_string(marker.as_std_path()).expect("read outside marker"),
        "precious-outside",
        "outside target must remain byte-identical"
    );
    assert!(
        !outside.join("tool").as_std_path().exists(),
        "link must not write bin/tool through the planted symlink"
    );
    assert!(
        !lexists(&p.join("opt/foo")),
        "no opt record when destination ancestry is unsafe"
    );
    assert!(
        !lexists(&p.join("var/homebrew/linked/foo")),
        "no linked record when destination ancestry is unsafe"
    );
}

// --- always-on destination ancestor confinement (MUST_EXIST_TOP / records) ---

#[test]
fn prefix_opt_symlink_blocks_link_and_outside_unchanged() {
    let fx = fixture();
    keg_file(&fx, "bin/tool", "keg-tool");

    let p = fx.prefix_path();
    let opt = p.join("opt");
    if opt.as_std_path().exists() {
        fs::remove_dir_all(opt.as_std_path()).expect("remove real opt");
    }
    let outside = utf8(fx._tmp.path().join("outside-prefix-opt"));
    fs::create_dir_all(outside.as_std_path()).expect("mkdir outside");
    let marker = outside.join("KEEPME");
    fs::write(marker.as_std_path(), "precious-opt-outside").expect("write outside marker");
    symlink(outside.as_std_path(), opt.as_std_path()).expect("plant prefix/opt symlink");

    let err = match link(&fx.keg, &fx.prefix, LinkOptions::default()) {
        Err(err) => err,
        Ok(report) => panic!("expected LinkConflict for prefix/opt symlink, got {report:?}"),
    };
    match err {
        PourError::LinkConflict { reason, target, .. } => {
            assert!(
                reason.contains("destination ancestor is a symlink") || reason.contains("symlink"),
                "reason={reason}"
            );
            assert_eq!(target, opt);
        }
        other => panic!("expected LinkConflict, got {other}"),
    }

    assert_eq!(
        fs::read_to_string(marker.as_std_path()).expect("read outside marker"),
        "precious-opt-outside",
        "outside target must remain byte-identical"
    );
    assert!(
        !outside.join("foo").as_std_path().exists(),
        "opt record must not be written through the planted symlink"
    );
    assert!(
        !lexists(&p.join("opt/foo")),
        "no opt record under planted symlink"
    );
    assert!(
        !lexists(&p.join("var/homebrew/linked/foo")),
        "no linked record when always-on destination ancestry is unsafe"
    );
}

#[test]
fn prefix_var_symlink_blocks_keg_only_and_outside_unchanged() {
    let fx = fixture();
    keg_file(&fx, "bin/tool", "keg-tool");

    let p = fx.prefix_path();
    let var = p.join("var");
    if var.as_std_path().exists() {
        fs::remove_dir_all(var.as_std_path()).expect("remove real var");
    }
    let outside = utf8(fx._tmp.path().join("outside-prefix-var"));
    fs::create_dir_all(outside.as_std_path()).expect("mkdir outside");
    let marker = outside.join("KEEPME");
    fs::write(marker.as_std_path(), "precious-var-outside").expect("write outside marker");
    symlink(outside.as_std_path(), var.as_std_path()).expect("plant prefix/var symlink");

    // keg-only skips LINK_DIRS classify(); always-on record preflight must still fire.
    let options = LinkOptions {
        keg_only: true,
        ..LinkOptions::default()
    };
    let err = match link(&fx.keg, &fx.prefix, options) {
        Err(err) => err,
        Ok(report) => panic!("expected LinkConflict for prefix/var symlink, got {report:?}"),
    };
    match err {
        PourError::LinkConflict { reason, target, .. } => {
            assert!(
                reason.contains("destination ancestor is a symlink") || reason.contains("symlink"),
                "reason={reason}"
            );
            assert_eq!(target, var);
        }
        other => panic!("expected LinkConflict, got {other}"),
    }

    assert_eq!(
        fs::read_to_string(marker.as_std_path()).expect("read outside marker"),
        "precious-var-outside",
        "outside target must remain byte-identical"
    );
    assert!(
        !outside.join("homebrew").as_std_path().exists(),
        "linked-record parents must not be created through the planted symlink"
    );
    assert!(
        !lexists(&p.join("opt/foo")),
        "no opt record when ancestry is unsafe"
    );
    assert!(
        !lexists(&p.join("var/homebrew/linked/foo")),
        "no linked record through planted var symlink"
    );
}
